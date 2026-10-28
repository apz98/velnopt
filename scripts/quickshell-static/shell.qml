// Comparison workload for scripts/measure.py: the panel of examples/hyprland.toml
// (36 px, three text groups, a minute clock) drawn with Quickshell.
import QtQuick
import Quickshell

ShellRoot {
    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property var modelData
            screen: modelData
            anchors { top: true; left: true; right: true }
            implicitHeight: 36
            color: "#152c2b"

            Text {
                anchors { left: parent.left; leftMargin: 18; verticalCenter: parent.verticalCenter }
                text: "Velnopt"
                color: "#96d1bd"
                font { family: "sans-serif"; pixelSize: 14 }
            }
            Text {
                anchors.centerIn: parent
                text: "Hyprland"
                color: "#e4ede6"
                font { family: "sans-serif"; pixelSize: 14 }
            }
            Text {
                id: clock
                anchors { right: parent.right; rightMargin: 18; verticalCenter: parent.verticalCenter }
                color: "#e4ede6"
                font { family: "sans-serif"; pixelSize: 14 }
                function update() { text = Qt.formatTime(new Date(), "HH:mm") }
                Component.onCompleted: update()
            }
            Timer {
                // wake at minute boundaries, like Velnopt's clock timer
                running: true
                repeat: true
                interval: 60000 - (Date.now() % 60000)
                onTriggered: { clock.update(); interval = 60000 - (Date.now() % 60000) }
            }
        }
    }
}
