//! Find applications from desktop entries, and launch them without a shell.
//!
//! Desktop entries are untrusted input. Values are bounded and sanitized, `Exec` is
//! split into an argument vector by the Desktop Entry quoting rules, and the program
//! is started directly. No shell ever sees an application name or value.

use std::{
    collections::HashSet,
    env,
    ffi::{CString, OsString},
    fs, io,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use nix::{
    fcntl::OFlag,
    spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn},
    sys::{signal::SigSet, stat::Mode},
    unistd::Pid,
};

const MAX_FILES: usize = 4096;
const MAX_APPS: usize = 2048;
const MAX_ENTRY_BYTES: u64 = 64 * 1024;
const MAX_ARGS: usize = 64;
const MAX_ARG_BYTES: usize = 4096;
const MAX_DEPTH: usize = 3;

/// An application you can launch, from a desktop entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct App {
    id: String,
    name: String,
    generic: Option<String>,
    keywords: Vec<String>,
    argv: Vec<String>,
}

impl App {
    /// Desktop file ID, such as `org.gnome.Nautilus.desktop`.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Localized display name: one line, at most 128 bytes.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Localized generic name, such as "Web Browser".
    pub fn generic(&self) -> Option<&str> {
        self.generic.as_deref()
    }
}

/// Discover applications in the XDG data directories of this session.
///
/// For a given desktop file ID, earlier directories win, and a hidden entry masks
/// the same ID in later directories. Entries are skipped if they are hidden, not
/// shown in this desktop, need a terminal, or have an invalid or missing `Exec`.
/// Results are sorted by name. Scanning stops at 4096 files of 64 KiB each.
pub fn discover() -> Vec<App> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| home.join(".local/share")));
    let data_dirs = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let roots: Vec<PathBuf> = data_home
        .into_iter()
        .chain(env::split_paths(&data_dirs).filter(|path| path.is_absolute()))
        .map(|path| path.join("applications"))
        .collect();
    let desktops: Vec<String> = env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let locale = env::var("LC_ALL")
        .or_else(|_| env::var("LC_MESSAGES"))
        .or_else(|_| env::var("LANG"))
        .unwrap_or_default();
    discover_in(&roots, &desktops, &locale, &|program| {
        on_path(program).is_some()
    })
}

fn discover_in(
    roots: &[PathBuf],
    desktops: &[String],
    locale: &str,
    available: &dyn Fn(&str) -> bool,
) -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    let mut budget = MAX_FILES;
    for root in roots {
        let mut files = Vec::new();
        collect(root, root, 0, &mut budget, &mut files);
        for (id, path) in files {
            if !seen.insert(id.clone()) || apps.len() == MAX_APPS {
                continue;
            }
            let Ok(file) = fs::File::open(&path) else {
                continue;
            };
            let mut source = String::new();
            if io::Read::read_to_string(&mut io::Read::take(file, MAX_ENTRY_BYTES + 1), &mut source)
                .is_err()
                || source.len() as u64 > MAX_ENTRY_BYTES
            {
                continue;
            }
            if let Some(app) = parse(id, &source, desktops, locale, available) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// Collect `*.desktop` files. As the spec says, IDs join subdirectories with `-`.
fn collect(
    root: &Path,
    directory: &Path,
    depth: usize,
    budget: &mut usize,
    files: &mut Vec<(String, PathBuf)>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < MAX_DEPTH {
            collect(root, &path, depth + 1, budget, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "desktop")
            && let Ok(relative) = path.strip_prefix(root)
            && let Some(id) = relative.to_str()
        {
            files.push((id.replace('/', "-"), path));
        }
    }
}

/// Parse the `[Desktop Entry]` group into an application that can be shown.
fn parse(
    id: String,
    source: &str,
    desktops: &[String],
    locale: &str,
    available: &dyn Fn(&str) -> bool,
) -> Option<App> {
    let mut in_entry = false;
    let mut values: Vec<(&str, &str)> = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_entry {
                break;
            }
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        values.push((key.trim(), value.trim()));
    }
    let raw = |key: &str| {
        values
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, value)| *value)
    };
    let localized = |key: &str| {
        let language = locale.split(['.', '@']).next().unwrap_or_default();
        let short = language.split('_').next().unwrap_or_default();
        [language, short]
            .into_iter()
            .filter(|tag| !tag.is_empty())
            .find_map(|tag| raw(&format!("{key}[{tag}]")))
            .or_else(|| raw(key))
            .map(unescape)
    };
    let flag = |key: &str| raw(key) == Some("true");
    let list = |key: &str| -> Vec<String> {
        raw(key)
            .map(|value| {
                unescape(value)
                    .split(';')
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    if raw("Type") != Some("Application") || flag("NoDisplay") || flag("Hidden") {
        return None;
    }
    // terminal apps need a terminal emulator, and v0.1 doesn't pick one
    if flag("Terminal") {
        return None;
    }
    let only = list("OnlyShowIn");
    if !only.is_empty() && !only.iter().any(|name| desktops.contains(name)) {
        return None;
    }
    if list("NotShowIn").iter().any(|name| desktops.contains(name)) {
        return None;
    }
    if let Some(try_exec) = raw("TryExec")
        && !available(&unescape(try_exec))
    {
        return None;
    }
    let name = sanitize(&localized("Name")?, 128);
    if name.trim().is_empty() {
        return None;
    }
    let argv = exec_argv(&unescape(raw("Exec")?), &name)?;
    Some(App {
        id,
        name,
        generic: localized("GenericName").map(|generic| sanitize(&generic, 128)),
        keywords: list("Keywords")
            .into_iter()
            .take(32)
            .map(|keyword| sanitize(&keyword, 64))
            .collect(),
        argv,
    })
}

/// Apply the general value escapes `\s`, `\n`, `\t`, `\r`, and `\\`.
fn unescape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        match characters.next() {
            Some('s') => result.push(' '),
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    result
}

/// Tokenize an `Exec` value into arguments, expanding or removing field codes.
///
/// File and URL codes are dropped, since the launcher never opens files. Unknown
/// field codes, unbalanced quotes, unquoted reserved characters, and oversized
/// commands make the entry invalid.
fn exec_argv(exec: &str, name: &str) -> Option<Vec<String>> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut characters = exec.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            ' ' | '\t' => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '"' => {
                started = true;
                loop {
                    match characters.next()? {
                        '"' => break,
                        '\\' => match characters.next()? {
                            escaped @ ('"' | '`' | '$' | '\\') => current.push(escaped),
                            _ => return None,
                        },
                        other => current.push(other),
                    }
                }
            }
            '%' => match characters.next()? {
                '%' => {
                    started = true;
                    current.push('%');
                }
                'c' => {
                    started = true;
                    current.push_str(name);
                }
                'f' | 'F' | 'u' | 'U' | 'i' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
                _ => return None,
            },
            '\'' | '\\' | '>' | '<' | '~' | '|' | '&' | ';' | '$' | '*' | '?' | '#' | '(' | ')'
            | '`' | '\n' | '\r' => return None,
            other => {
                started = true;
                current.push(other);
            }
        }
        if current.len() > MAX_ARG_BYTES || argv.len() >= MAX_ARGS {
            return None;
        }
    }
    if started {
        argv.push(current);
    }
    (!argv.is_empty() && !argv[0].is_empty()).then_some(argv)
}

/// Find a program the way `execvp` would, without running it.
fn on_path(program: &str) -> Option<PathBuf> {
    let executable = |path: &Path| {
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        let path = PathBuf::from(program);
        return (path.is_absolute() && executable(&path)).then_some(path);
    }
    env::split_paths(&env::var_os("PATH")?)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .find(|path| executable(path))
}

fn sanitize(source: &str, limit: usize) -> String {
    let mut result = String::with_capacity(source.len().min(limit));
    for character in source.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if result.len() + character.len_utf8() > limit {
            result.truncate(result.floor_char_boundary(limit.saturating_sub(3)));
            result.push('…');
            break;
        }
        result.push(character);
    }
    result
}

/// Rank applications for a query, best first, returning at most `limit` indices.
///
/// Matching ignores case. Name prefixes rank first, then word prefixes, then
/// name substrings, then generic-name or keyword substrings. An empty query keeps
/// alphabetical order.
pub fn search(apps: &[App], query: &str, limit: usize) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(index, app)| {
            if query.is_empty() {
                return Some((0, index));
            }
            let name = app.name.to_lowercase();
            let rank = if name.starts_with(&query) {
                0
            } else if name
                .split(|character: char| !character.is_alphanumeric())
                .any(|word| word.starts_with(&query))
            {
                1
            } else if name.contains(&query) {
                2
            } else if app
                .generic
                .iter()
                .chain(&app.keywords)
                .any(|text| text.to_lowercase().contains(&query))
            {
                3
            } else {
                return None;
            };
            Some((rank, index))
        })
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, index)| index)
        .collect()
}

/// Start an application directly, detached in its own process group.
///
/// The child gets an empty signal mask and default signal handling, so the panel's
/// blocked signals (it reads them through a signalfd) don't leak into applications.
/// Standard streams are `/dev/null`, and the environment and working directory
/// are inherited. Call [`reap`] when `SIGCHLD` arrives so finished children
/// don't linger.
///
/// # Errors
///
/// Returns an error when the program cannot be found or started.
pub fn launch(app: &App) -> io::Result<()> {
    let program = on_path(&app.argv[0]).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not an executable on PATH", app.argv[0]),
        )
    })?;
    let nul = |_| io::Error::new(io::ErrorKind::InvalidInput, "argument contains a NUL byte");
    let path = CString::new(program.into_os_string().into_vec()).map_err(nul)?;
    let argv = app
        .argv
        .iter()
        .map(|argument| CString::new(argument.as_bytes()).map_err(nul))
        .collect::<io::Result<Vec<_>>>()?;
    let environment: Vec<CString> = env::vars_os()
        .filter_map(|(key, value)| {
            let mut entry: OsString = key;
            entry.push("=");
            entry.push(value);
            CString::new(entry.into_vec()).ok()
        })
        .collect();
    let mut actions = PosixSpawnFileActions::init()?;
    actions.add_open(0, "/dev/null", OFlag::O_RDONLY, Mode::empty())?;
    for stream in [1, 2] {
        actions.add_open(stream, "/dev/null", OFlag::O_WRONLY, Mode::empty())?;
    }
    let mut attributes = PosixSpawnAttr::init()?;
    attributes.set_flags(
        PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK
            | PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF
            | PosixSpawnFlags::POSIX_SPAWN_SETPGROUP,
    )?;
    attributes.set_sigmask(&SigSet::empty())?;
    attributes.set_sigdefault(&SigSet::all())?;
    attributes.set_pgroup(Pid::from_raw(0))?;
    // `reap` collects the child on SIGCHLD, so we don't need its process ID
    posix_spawn(path.as_c_str(), &actions, &attributes, &argv, &environment)?;
    Ok(())
}

/// Reap every child that has exited, without blocking.
pub fn reap() {
    while let Ok(Some(_)) = rustix::process::waitpid(None, rustix::process::WaitOptions::NOHANG) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;

    fn entry(body: &str) -> Option<App> {
        parse(
            "test.desktop".into(),
            &format!("[Desktop Entry]\nType=Application\n{body}"),
            &["Hyprland".into()],
            "de_DE.UTF-8",
            &|program| program != "missing",
        )
    }

    #[test]
    fn tokenizes_exec_without_a_shell_and_handles_field_codes() {
        assert_eq!(exec_argv(r#"firefox %u"#, "Firefox").unwrap(), ["firefox"]);
        assert_eq!(
            exec_argv(
                r#""/opt/My App/run" --name=%c "a \"quoted\" \$HOME" 100%%"#,
                "Viewer"
            )
            .unwrap(),
            [
                "/opt/My App/run",
                "--name=Viewer",
                r#"a "quoted" $HOME"#,
                "100%"
            ]
        );
        assert_eq!(exec_argv(r#"app """#, "x").unwrap(), ["app", ""]);
        for invalid in [
            "",
            "   ",
            r#"app "unterminated"#,
            "app %z",
            "app; rm -rf ~",
            "app $(id)",
            "app `id`",
            "app > /tmp/x",
            "app | tee",
            "app 'single'",
            r#"app "\n""#,
            "%",
        ] {
            assert_eq!(exec_argv(invalid, "x"), None, "{invalid:?}");
        }
        let many = format!("app{}", " a".repeat(MAX_ARGS + 1));
        assert_eq!(exec_argv(&many, "x"), None);
    }

    #[test]
    fn parses_visible_localized_entries_and_skips_hidden_ones() {
        let app = entry("Name=Files\nName[de]=Dateien\nGenericName=File Manager\nKeywords=folder;explorer;\nExec=nautilus --new-window %U\n[Desktop Action new]\nExec=evil").unwrap();
        assert_eq!(app.name(), "Dateien");
        assert_eq!(app.generic(), Some("File Manager"));
        assert_eq!(app.keywords, ["folder", "explorer"]);
        assert_eq!(app.argv, ["nautilus", "--new-window"]);
        for hidden in [
            "Name=A\nExec=a\nNoDisplay=true",
            "Name=A\nExec=a\nHidden=true",
            "Name=A\nExec=a\nTerminal=true",
            "Name=A\nExec=a\nOnlyShowIn=GNOME;KDE;",
            "Name=A\nExec=a\nNotShowIn=Hyprland;",
            "Name=A\nExec=a\nTryExec=missing",
            "Name=A",
            "Exec=a",
            "Name= \nExec=a",
            "Name=A\nExec=a; reboot",
        ] {
            assert_eq!(entry(hidden), None, "{hidden:?}");
        }
        assert!(entry("Name=A\nExec=a\nOnlyShowIn=Hyprland;").is_some());
        assert_eq!(
            parse(
                "x.desktop".into(),
                "[Desktop Entry]\nType=Link\nName=A\nExec=a",
                &[],
                "",
                &|_| true
            ),
            None
        );
        let control = entry("Name=Bad\\nName\u{7}\nExec=a").unwrap();
        assert_eq!(control.name(), "Bad Name ");
        let long = entry(&format!("Name={}\nExec=a", "🦀".repeat(100))).unwrap();
        assert!(long.name().len() <= 128 && long.name().ends_with('…'));
    }

    #[test]
    fn discovery_respects_precedence_masking_subdirectories_and_bounds() {
        let base = env::temp_dir().join(format!("velnopt-apps-{}", std::process::id()));
        let user = base.join("user");
        let system = base.join("system");
        for directory in [&user, &system.join("vendor")] {
            fs::create_dir_all(directory).unwrap();
        }
        let write = |path: PathBuf, name: &str, extra: &str| {
            fs::write(
                path,
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec=run-{name}\n{extra}"),
            )
            .unwrap();
        };
        write(user.join("editor.desktop"), "Mine", "");
        write(system.join("editor.desktop"), "System", "");
        write(user.join("masked.desktop"), "Masked", "Hidden=true");
        write(system.join("masked.desktop"), "Shown", "");
        write(system.join("vendor/tool.desktop"), "Tool", "");
        fs::write(
            system.join("huge.desktop"),
            vec![b'#'; MAX_ENTRY_BYTES as usize + 1],
        )
        .unwrap();
        fs::write(system.join("notes.txt"), "Name=Nope").unwrap();
        let apps = discover_in(&[user, system], &[], "", &|_| true);
        fs::remove_dir_all(&base).unwrap();
        let names: Vec<_> = apps.iter().map(App::name).collect();
        assert_eq!(names, ["Mine", "Tool"]);
        assert_eq!(apps[1].id(), "vendor-tool.desktop");
    }

    #[test]
    fn search_ranks_prefixes_words_substrings_and_keywords() {
        let app = |name: &str, generic: Option<&str>, keywords: &[&str]| App {
            id: format!("{name}.desktop"),
            name: name.into(),
            generic: generic.map(str::to_owned),
            keywords: keywords
                .iter()
                .map(|keyword| (*keyword).to_owned())
                .collect(),
            argv: vec!["x".into()],
        };
        let apps = [
            app("Firefox", Some("Web Browser"), &[]),
            app("GNU Image Manipulation Program", None, &["photo"]),
            app("Files", Some("File Manager"), &["folder"]),
            app("Wireshark", None, &[]),
        ];
        assert_eq!(search(&apps, "fi", 8), [0, 2]);
        assert_eq!(search(&apps, "IMAGE", 8), [1]);
        assert_eq!(search(&apps, "shar", 8), [3]);
        assert_eq!(search(&apps, "browser", 8), [0]);
        assert_eq!(search(&apps, "folder", 8), [2]);
        assert_eq!(search(&apps, "", 2), [0, 1]);
        assert!(search(&apps, "zzz", 8).is_empty());
    }

    #[test]
    fn launched_children_get_an_empty_signal_mask() {
        let marker = env::temp_dir().join(format!("velnopt-sigmask-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let app = App {
            id: "mask.desktop".into(),
            name: "mask".into(),
            generic: None,
            keywords: Vec::new(),
            argv: exec_argv(
                &format!(
                    "sh -c \"grep SigBlk /proc/self/status > '{}'\"",
                    marker.display()
                ),
                "x",
            )
            .unwrap(),
        };
        // block signals in this thread, as the panel's signalfd setup does
        std::thread::spawn(move || {
            let mut blocked = SigSet::empty();
            blocked.add(Signal::SIGTERM);
            blocked.add(Signal::SIGUSR1);
            blocked.thread_block().unwrap();
            launch(&app).unwrap();
        })
        .join()
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !fs::read_to_string(&marker).is_ok_and(|text| text.ends_with('\n')) {
            assert!(std::time::Instant::now() < deadline, "child never ran");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let status = fs::read_to_string(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        reap();
        assert_eq!(
            status.split_whitespace().collect::<Vec<_>>(),
            ["SigBlk:", "0000000000000000"]
        );
    }

    #[test]
    fn launches_directly_without_shell_interpolation() {
        let marker = env::temp_dir().join(format!("velnopt-launch-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        // a name full of shell syntax still arrives as one literal argument
        let app = App {
            id: "touch.desktop".into(),
            name: "$(reboot); `id`".into(),
            generic: None,
            keywords: Vec::new(),
            argv: exec_argv(&format!("touch \"{}\"", marker.display()), "x").unwrap(),
        };
        launch(&app).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() {
            assert!(std::time::Instant::now() < deadline, "child never ran");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        reap();
        fs::remove_file(&marker).unwrap();
        let missing = App {
            argv: vec!["velnopt-definitely-missing-program".into()],
            ..app
        };
        assert_eq!(
            launch(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
