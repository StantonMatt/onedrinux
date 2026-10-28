import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.kirigamiaddons.formcard as FormCard
import org.konedrive.app

/// The whole app's settings: start at login, download progress, Places,
/// when every account pauses by itself (metered connections, battery), the
/// download guard with the apps it blocked or has a rule for, and Quit. Each
/// OneDrive's own things (its folder, its sign-in, its previews) are on its
/// OneDrive Settings page.
FormCard.FormCardPage {
    id: page

    readonly property bool available: Daemon.serviceAvailable
    /// The guard is there and on: its limits and its apps show.
    readonly property bool guardOn: DownloadGuard.serviceAvailable && DownloadGuard.enabled

    /// The apps the download guard blocked brought into sight: where its notification leads.
    function showGuardApps() {
        Qt.callLater(() => {
            const target = guardAppsHeader.visible ? guardAppsHeader : guardSwitch;
            const y = target.mapToItem(flickable.contentItem, 0, 0).y;
            flickable.contentY = Math.max(0, Math.min(y, flickable.contentHeight - flickable.height));
        });
    }

    objectName: "settingsPage"
    title: i18nc("@title", "Settings")

    Kirigami.InlineMessage {
        objectName: "settingsError"
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.largeSpacing
        Layout.leftMargin: Kirigami.Units.largeSpacing
        Layout.rightMargin: Kirigami.Units.largeSpacing
        type: Kirigami.MessageType.Error
        text: [Daemon.actionError, DownloadGuard.actionError].filter(error => error.length > 0).join("\n")
        visible: text.length > 0
    }

    // The app itself
    FormCard.FormHeader {
        title: i18nc("@title:group", "App")
    }
    FormCard.FormCard {
        FormCard.FormSwitchDelegate {
            id: startAtLogin
            objectName: "startAtLogin"
            text: i18n("Start at login")
            description: i18n("KOneDrive starts in the system tray when you log in. Notifications come from it, so with it closed nothing notifies.")
            checked: Autostart.enabled
            onToggled: {
                Autostart.enabled = checked;
                // The entry is the truth: after a failure the switch goes back.
                checked = Qt.binding(() => Autostart.enabled);
            }
        }
        Kirigami.InlineMessage {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            type: Kirigami.MessageType.Error
            text: Autostart.error
            visible: text.length > 0
        }
        FormCard.FormDelegateSeparator {}
        FormCard.FormSwitchDelegate {
            id: showDownloadProgress
            objectName: "showDownloadProgress"
            text: i18n("Show download and upload progress")
            description: i18n("A long download or upload shows its progress in Plasma, the same way Dolphin's copying does.")
            checked: DownloadProgress.enabled
            onToggled: DownloadProgress.enabled = checked
        }
        FormCard.FormDelegateSeparator {}
        FormCard.FormSwitchDelegate {
            id: showInPlaces
            objectName: "showInPlaces"
            text: i18n("Show in Places")
            description: i18n("Each account's OneDrive folder gets an entry named after the account in Dolphin's Places panel and in file dialogs.")
            checked: Places.enabled
            onToggled: Places.enabled = checked
        }
        FormCard.FormDelegateSeparator {}
        FormCard.FormSwitchDelegate {
            id: trayIconPerAccount
            objectName: "trayIconPerAccount"
            text: i18n("Show a tray icon for each account")
            description: i18n("With several accounts, each has its own icon with its own state and menu. When off, one icon shows the most serious state of them all.")
            checked: Tray.perAccount
            onToggled: Tray.perAccount = checked
        }
    }

    // When every account holds back by itself (issue #95): the daemon's settings, written
    // to config.toml; the controls show what it says, and a change goes through it.
    FormCard.FormHeader {
        title: i18nc("@title:group", "Sync")
    }
    FormCard.FormCard {
        objectName: "holdSettings"

        FormCard.FormSwitchDelegate {
            id: meteredSwitch
            objectName: "meteredSwitch"

            readonly property bool on: Daemon.pauseOnMetered

            text: i18n("Pause on metered connections")
            description: i18n("On a connection marked as metered, such as a phone's hotspot, nothing is uploaded or downloaded in the background. Opening a file still downloads it.")
            enabled: page.available
            checked: on
            // The inner switch writes `checked` back, ending the binding: kept in step here.
            onOnChanged: checked = on
            onToggled: {
                const wanted = checked;
                checked = on;
                Daemon.setPauseOnMetered(wanted);
            }
        }
        FormCard.FormDelegateSeparator {}
        // A combo box of its own, laid out as FormComboBoxDelegate's, which turns with the
        // wheel: scrolling the page over it would change the setting.
        FormCard.AbstractFormDelegate {
            id: batteryRow
            enabled: page.available
            focusPolicy: Qt.NoFocus
            background: null
            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing
                QQC2.Label {
                    Layout.fillWidth: true
                    text: i18n("On battery")
                    wrapMode: Text.Wrap
                    color: batteryRow.enabled ? Kirigami.Theme.textColor : Kirigami.Theme.disabledTextColor
                    Accessible.ignored: true
                }
                QQC2.ComboBox {
                    id: batteryCombo
                    objectName: "batteryCombo"

                    readonly property string choice: Daemon.onBattery
                    readonly property var choices: ["sync", "power-saver", "pause"]

                    Layout.fillWidth: true
                    wheelEnabled: false
                    Accessible.name: i18n("On battery")
                    model: [i18nc("@item:inlistbox on battery", "Sync as usual"), i18nc("@item:inlistbox on battery", "Pause in power-saver mode"), i18nc("@item:inlistbox on battery", "Pause")]
                    currentIndex: Math.max(0, choices.indexOf(choice))
                    onChoiceChanged: currentIndex = Math.max(0, choices.indexOf(choice))
                    onActivated: index => {
                        currentIndex = Math.max(0, choices.indexOf(choice));
                        Daemon.setOnBattery(choices[index]);
                    }
                }
            }
        }
        // The download guard (docs/design/download-guard.md): the daemon's, for every account.
        // A daemon from before it has none to show; one that is away, the switch as it was.
        FormCard.FormDelegateSeparator {
            visible: guardSwitch.visible
        }
        FormCard.FormSwitchDelegate {
            id: guardSwitch
            objectName: "guardSwitch"

            readonly property bool on: DownloadGuard.enabled

            visible: DownloadGuard.serviceAvailable || !page.available
            text: i18n("Block bulk downloads")
            description: DownloadGuard.description
            enabled: DownloadGuard.serviceAvailable
            checked: on
            // The inner switch writes `checked` back, ending the binding: kept in step here.
            onOnChanged: checked = on
            onToggled: {
                const wanted = checked;
                checked = on;
                DownloadGuard.setEnabled(wanted);
            }
        }
        FormCard.FormButtonDelegate {
            objectName: "changeLimitsButton"
            visible: page.guardOn
            text: i18nc("@action:button", "Change Limits…")
            icon.name: "configure"
            onClicked: limitsDialog.open()
        }
        FormCard.FormTextDelegate {
            objectName: "guardOffLine"
            visible: DownloadGuard.serviceAvailable && !DownloadGuard.enabled
            text: i18n("Any app can download your whole OneDrive.")
            textItem.wrapMode: Text.Wrap
            leading: Kirigami.Icon {
                source: "dialog-warning"
                implicitWidth: Kirigami.Units.iconSizes.small
                implicitHeight: Kirigami.Units.iconSizes.small
            }
        }
    }

    // The apps the guard blocked or has a rule for; only while there is one. Turned off, the
    // guard keeps its rules, and the list comes back with it.
    FormCard.FormHeader {
        id: guardAppsHeader
        objectName: "guardAppsHeader"
        visible: guardAppsCard.visible
        title: i18nc("@title:group", "Blocked and Allowed Apps")
    }
    FormCard.FormCard {
        id: guardAppsCard
        objectName: "guardAppsCard"
        visible: page.guardOn && DownloadGuard.apps.length > 0

        // What the last Free Up What It Downloaded did, until it is closed.
        Kirigami.InlineMessage {
            objectName: "guardFreeUpMessage"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            visible: text.length > 0
            type: Kirigami.MessageType.Information
            text: DownloadGuard.freeUpResult
            actions: Kirigami.Action {
                text: i18nc("@action:button", "Close")
                icon.name: "dialog-close"
                displayHint: Kirigami.DisplayHint.IconOnly
                onTriggered: DownloadGuard.clearFreeUpResult()
            }
        }
        Repeater {
            model: DownloadGuard.apps
            delegate: ColumnLayout {
                required property var modelData
                required property int index

                Layout.fillWidth: true
                spacing: 0

                FormCard.FormDelegateSeparator {
                    visible: parent.index > 0
                }
                GuardAppRow {
                    Layout.fillWidth: true
                    app: parent.modelData
                }
            }
        }
    }

    FormCard.FormCard {
        Layout.topMargin: Kirigami.Units.largeSpacing * 2

        FormCard.FormButtonDelegate {
            objectName: "quitButton"
            text: i18nc("@action:button", "Quit KOneDrive")
            description: i18n("Closes the window and the tray icon. Your files stay; nothing notifies until KOneDrive starts again.")
            icon.name: "application-exit"
            onClicked: Qt.quit()
        }
    }
    // The limits, all three written together. The boxes take what is typed as it is typed:
    // Save stays off until a number in range differs from what the daemon keeps.
    Kirigami.PromptDialog {
        id: limitsDialog
        objectName: "limitsDialog"

        /// The number typed in `box`, or NaN when it is none within the box's range.
        function typed(box, text) {
            let value = NaN;
            try {
                value = box.valueFromText(text, box.locale);
            } catch (error) {
                return NaN;
            }
            return Number.isInteger(value) && value >= box.from && value <= box.to ? value : NaN;
        }

        // CLI limits can exceed these controls' ranges. An unchanged box keeps
        // the daemon's value, including byte/second precision, when another is saved.
        property real shownFiles: 0
        property real shownGiB: 0
        property real shownMinutes: 0
        readonly property real typedFiles: typed(filesBox, filesBox.contentItem.text)
        readonly property real typedGiB: typed(gibBox, gibBox.contentItem.text)
        readonly property real typedMinutes: typed(minutesBox, minutesBox.contentItem.text)
        readonly property real files: typedFiles === shownFiles ? DownloadGuard.maxFiles : typedFiles
        readonly property real gib: typedGiB === shownGiB ? DownloadGuard.maxGiB : typedGiB
        readonly property real minutes: typedMinutes === shownMinutes ? DownloadGuard.windowMinutes : typedMinutes
        readonly property bool valid: !isNaN(files) && !isNaN(gib) && !isNaN(minutes)
        readonly property bool differs: files !== DownloadGuard.maxFiles || gib !== DownloadGuard.maxGiB || minutes !== DownloadGuard.windowMinutes

        function save() {
            if (!valid || !differs) {
                return;
            }
            DownloadGuard.setLimits(files, gib, minutes);
            close();
        }

        /// `box` holding `value`, also when what was typed into it last time was never taken.
        function showValue(box, value) {
            box.value = value;
            box.contentItem.text = box.textFromValue(box.value, box.locale);
        }

        onAboutToShow: {
            showValue(filesBox, DownloadGuard.maxFiles);
            showValue(gibBox, DownloadGuard.maxGiB);
            showValue(minutesBox, DownloadGuard.windowMinutes);
            shownFiles = filesBox.value;
            shownGiB = gibBox.value;
            shownMinutes = minutesBox.value;
            filesBox.forceActiveFocus();
        }
        title: i18nc("@title:window", "Bulk Download Limits")
        standardButtons: Kirigami.Dialog.NoButton
        customFooterActions: [
            Kirigami.Action {
                objectName: "saveLimits"
                text: i18nc("@action:button", "Save")
                icon.name: "document-save"
                enabled: limitsDialog.valid && limitsDialog.differs
                onTriggered: limitsDialog.save()
            },
            Kirigami.Action {
                text: i18nc("@action:button", "Cancel")
                icon.name: "dialog-cancel"
                onTriggered: limitsDialog.close()
            }
        ]

        ColumnLayout {
            spacing: Kirigami.Units.smallSpacing

            QQC2.Label {
                Layout.fillWidth: true
                text: i18n("Block an app after more than this many files")
                wrapMode: Text.Wrap
                Accessible.ignored: true
            }
            QQC2.SpinBox {
                id: filesBox
                objectName: "maxFilesBox"
                Layout.fillWidth: true
                from: 1
                to: 10000
                editable: true
                Accessible.name: i18n("Block an app after more than this many files")
                Keys.onReturnPressed: limitsDialog.save()
                Keys.onEnterPressed: limitsDialog.save()
            }
            QQC2.Label {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing
                text: i18n("Or more than this much, in GiB")
                wrapMode: Text.Wrap
                Accessible.ignored: true
            }
            QQC2.SpinBox {
                id: gibBox
                objectName: "maxGiBBox"
                Layout.fillWidth: true
                from: 1
                to: 1000
                editable: true
                Accessible.name: i18n("Or more than this much, in GiB")
                Keys.onReturnPressed: limitsDialog.save()
                Keys.onEnterPressed: limitsDialog.save()
            }
            QQC2.Label {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing
                text: i18n("Within this many minutes")
                wrapMode: Text.Wrap
                Accessible.ignored: true
            }
            QQC2.SpinBox {
                id: minutesBox
                objectName: "windowMinutesBox"
                Layout.fillWidth: true
                from: 1
                to: 1440
                editable: true
                Accessible.name: i18n("Within this many minutes")
                Keys.onReturnPressed: limitsDialog.save()
                Keys.onEnterPressed: limitsDialog.save()
            }
        }

        // The guard gone with the daemon, or turned off: the limits are not the question now.
        Connections {
            target: DownloadGuard
            function onChanged() {
                if (!page.guardOn) {
                    limitsDialog.close();
                }
            }
        }
    }
}
