import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.kirigamiaddons.formcard as FormCard
import org.konedrive.app

/// The settings of the OneDrive chosen in the sidebar — an account, its folder and how it
/// syncs, as one: its name and Microsoft account, two-way sync and a trial's folders that can be
/// changed, its folder (or another one),
/// its previews and the free space its downloads leave, the advanced settings (what is never
/// uploaded, signing out), and disconnecting it. Signing in again and opening the folder are
/// on Status.
FormCard.FormCardPage {
    id: page

    readonly property var account: Current.account
    readonly property var sync: Current.sync
    // Current.account and Current.sync change together, but these bindings
    // may see one before the other: each checks both.
    readonly property bool available: account !== null && sync !== null && account.serviceAvailable && sync.serviceAvailable
    readonly property bool signedIn: available && account.state === "signed-in"
    readonly property bool hasFolder: available && sync.rootPath.length > 0
    readonly property string accountError: account ? (account.actionError.length > 0 ? account.actionError : account.lastError) : ""
    readonly property var window: QQC2.ApplicationWindow.window
    /// Large changes wait for review (Settings' Hold large changes, the whole app's).
    readonly property bool holdOn: ChangeGuard.serviceAvailable && ChangeGuard.enabled

    /// The Sync card brought into sight: where a refused change's notification leads.
    function showSyncSettings() {
        Qt.callLater(() => {
            if (!syncHeader.visible) {
                return;
            }
            const y = syncHeader.mapToItem(flickable.contentItem, 0, 0).y;
            flickable.contentY = Math.max(0, Math.min(y, flickable.contentHeight - flickable.height));
        });
    }

    objectName: "oneDriveSettingsPage"
    title: window ? window.accountTitle(i18nc("@title", "OneDrive Settings")) : i18nc("@title", "OneDrive Settings")

    Kirigami.InlineMessage {
        objectName: "accountError"
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.largeSpacing
        Layout.leftMargin: Kirigami.Units.largeSpacing
        Layout.rightMargin: Kirigami.Units.largeSpacing
        type: Kirigami.MessageType.Error
        text: page.accountError
        visible: page.available && text.length > 0
    }
    Kirigami.InlineMessage {
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.largeSpacing
        Layout.leftMargin: Kirigami.Units.largeSpacing
        Layout.rightMargin: Kirigami.Units.largeSpacing
        type: Kirigami.MessageType.Error
        text: page.sync ? page.sync.actionError : ""
        visible: page.available && text.length > 0
    }
    // A Remove of this account that was refused.
    Kirigami.InlineMessage {
        objectName: "removeError"
        Layout.fillWidth: true
        Layout.topMargin: Kirigami.Units.largeSpacing
        Layout.leftMargin: Kirigami.Units.largeSpacing
        Layout.rightMargin: Kirigami.Units.largeSpacing
        type: Kirigami.MessageType.Error
        text: i18n("This OneDrive was not disconnected: %1", Daemon.removeError)
        visible: page.available && page.account !== null && Daemon.removeFailedPath === page.account.path
    }

    // The account: its name here, and the Microsoft account behind it.
    FormCard.FormHeader {
        visible: page.available
        title: i18nc("@title:group", "Account")
    }
    FormCard.FormCard {
        visible: page.available

        FormCard.FormTextDelegate {
            text: i18nc("@label", "Name")
            description: page.account ? page.account.label : ""
            trailing: QQC2.Button {
                objectName: "renameButton"
                text: i18nc("@action:button", "Rename…")
                icon.name: "edit-rename"
                onClicked: renameDialog.open()
            }
        }
        FormCard.FormDelegateSeparator {
            visible: page.signedIn
        }
        FormCard.FormTextDelegate {
            visible: page.signedIn
            text: page.account && page.account.displayName.length > 0 ? page.account.displayName : i18n("Microsoft account")
            description: page.account ? page.account.email : ""
            trailing: QQC2.Button {
                text: i18nc("@action:button", "Refresh")
                icon.name: "view-refresh"
                display: QQC2.AbstractButton.IconOnly
                QQC2.ToolTip.text: i18nc("@info:tooltip", "Read the name and the storage again")
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                onClicked: page.account.refreshAccountInfo()
            }
        }
    }

    // How the folder syncs: the account's mode (Account.Mode). While a switch is under way the
    // switch shows the mode asked for, and what it waits for is below it with its ways out.
    // Two-way, a trial can allow changes only in folders chosen (Account.WritePaths).
    FormCard.FormHeader {
        id: syncHeader
        visible: page.hasFolder
        title: i18nc("@title:group", "Sync")
    }
    FormCard.FormCard {
        objectName: "syncSettings"
        visible: page.hasFolder

        FormCard.FormSwitchDelegate {
            id: twoWaySwitch
            objectName: "twoWaySwitch"

            /// The account syncs two-way.
            readonly property bool on: page.account !== null && page.account.mode === "read-write"
            /// A switch to two-way is under way: asked for, or waiting for the browser.
            readonly property bool towardTwoWay: page.account !== null && page.account.switchingTo === "read-write"
            /// A switch to read-only is under way: asked for, or waiting for what is unsent.
            readonly property bool towardReadOnly: page.account !== null && (page.account.switchingTo === "read-only" || page.account.switchingToReadOnly)
            /// What the switch shows: the mode asked for while a switch is under way.
            readonly property bool shown: towardTwoWay || (on && !towardReadOnly)

            text: i18n("Two-way sync")
            description: {
                if (!shown) {
                    const readOnly = i18n("Your files download and open as usual, but can't be changed here, and nothing is uploaded.");
                    return on ? readOnly : readOnly + " " + i18n("Turning it on opens your browser once more to allow KOneDrive to change your files.");
                }
                if (on && page.sync !== null && !page.sync.writable) {
                    return i18n("Two-way sync is on, but the folder can't be watched right now, so nothing is uploaded. If it stays so, Status says why.");
                }
                if (page.holdOn) {
                    return i18n("What you add, change, move or delete here happens in OneDrive too. Large changes wait for your review, and deleted files go to OneDrive's recycle bin.");
                }
                return i18n("What you add, change, move or delete here happens in OneDrive too. Deleted files go to OneDrive's recycle bin.");
            }
            enabled: page.signedIn && !towardTwoWay && !towardReadOnly
            checked: shown
            // The inner switch writes `checked` back, ending the binding: kept in step here.
            onShownChanged: checked = shown
            onToggled: {
                const wanted = checked;
                checked = shown;
                page.account.setMode(wanted ? "read-write" : "read-only", false);
            }
        }
        FormCard.FormTextDelegate {
            objectName: "modeSwitchLine"
            visible: twoWaySwitch.towardTwoWay || twoWaySwitch.towardReadOnly
            text: {
                if (page.account === null) {
                    return "";
                }
                if (page.account.modeSignInPending) {
                    return i18n("Waiting for you to allow KOneDrive to change your files in your browser…");
                }
                if (twoWaySwitch.towardTwoWay) {
                    return i18n("Starting the sign-in…");
                }
                if (page.account.switchingToReadOnly) {
                    return i18np("Switching to read-only: 1 change still to send. The folder turns read-only by itself once it is uploaded.",
                                 "Switching to read-only: %1 changes still to send. The folder turns read-only by itself once they are uploaded.",
                                 page.account.unsentCount);
                }
                return i18n("Switching to read-only…");
            }
            textItem.wrapMode: Text.Wrap
            icon.name: "view-refresh"
        }
        FormCard.FormButtonDelegate {
            visible: page.account !== null && page.account.modeSignInPending
            enabled: page.account !== null && page.account.signInUrl.length > 0
            text: i18nc("@action:button", "Copy Sign-In Link")
            icon.name: "edit-copy"
            onClicked: page.account.copySignInUrl()
        }
        FormCard.FormButtonDelegate {
            objectName: "cancelTwoWayButton"
            visible: page.account !== null && page.account.modeSignInPending
            text: i18nc("@action:button", "Cancel")
            icon.name: "dialog-cancel"
            onClicked: page.account.cancelModeSwitch()
        }
        FormCard.FormButtonDelegate {
            objectName: "switchToReadOnlyNow"
            visible: page.account !== null && page.account.switchingToReadOnly
            enabled: page.account !== null && page.account.switchingTo.length === 0
            text: i18nc("@action:button", "Switch Now")
            description: i18n("What is not uploaded stays on this device, and is uploaded once two-way sync is on again.")
            icon.name: "object-locked"
            onClicked: page.account.setMode("read-only", true)
        }
        FormCard.FormButtonDelegate {
            objectName: "cancelReadOnlySwitch"
            visible: page.account !== null && page.account.switchingToReadOnly
            enabled: page.account !== null && page.account.switchingTo.length === 0
            text: i18nc("@action:button", "Keep Two-Way Sync")
            icon.name: "dialog-cancel"
            onClicked: page.account.setMode("read-write", false)
        }
        // Settings' Hold large changes is off: nothing waits for review.
        FormCard.FormTextDelegate {
            objectName: "notHeldLine"
            visible: twoWaySwitch.on && twoWaySwitch.shown && ChangeGuard.serviceAvailable && !ChangeGuard.enabled
            text: i18n("Large changes are not held for review: “Hold large changes” is off in Settings.")
            textItem.wrapMode: Text.Wrap
            leading: Kirigami.Icon {
                source: "dialog-warning"
                implicitWidth: Kirigami.Units.iconSizes.small
                implicitHeight: Kirigami.Units.iconSizes.small
            }
        }

        // A trial: changes only in the folders listed; the rest of the OneDrive stays read-only
        // here. Only two-way, where changes are made at all. Turning it on picks its first
        // folder; its last folder can't be removed; turning it off asks first. Each change goes
        // through the daemon, and the rows show what it says.
        FormCard.FormDelegateSeparator {
            visible: trialSwitch.visible
        }
        FormCard.FormSwitchDelegate {
            id: trialSwitch
            objectName: "trialSwitch"

            readonly property bool on: page.account !== null && page.account.writeScopeEnabled

            visible: twoWaySwitch.on && twoWaySwitch.shown
            text: i18n("Only these folders can be changed")
            description: on ? i18n("The rest of your OneDrive stays read-only on this computer.")
                            : i18n("For a trial: changes are allowed only in folders you choose, and the rest of your OneDrive stays read-only here.")
            enabled: page.available && !page.account.writeScopeChanging
            checked: on
            // The inner switch writes `checked` back, ending the binding: kept in step here.
            onOnChanged: checked = on
            onToggled: {
                const wanted = checked;
                checked = on;
                if (wanted) {
                    trialFolderDialog.openFor(page.account, page.sync);
                } else {
                    allowEverywhereDialog.open();
                }
            }
        }
        Repeater {
            model: trialSwitch.visible && trialSwitch.on && page.account ? page.account.writePaths : []
            delegate: FormCard.FormTextDelegate {
                id: folderRow
                objectName: "trialFolder"

                required property string modelData
                /// The last folder stays: removing it would refuse every change.
                readonly property bool last: page.account !== null && page.account.writePaths.length <= 1

                text: modelData
                textItem.textFormat: Text.PlainText
                textItem.elide: Text.ElideMiddle
                icon.name: "folder"

                HoverHandler {
                    id: folderHover
                }
                QQC2.ToolTip.text: modelData.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
                QQC2.ToolTip.visible: folderHover.hovered && folderRow.textItem.truncated && !removeHover.hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

                // A disabled button takes no hover: the tooltip is its holder's.
                trailing: Item {
                    implicitWidth: removeButton.implicitWidth
                    implicitHeight: removeButton.implicitHeight

                    HoverHandler {
                        id: removeHover
                    }
                    QQC2.ToolTip.text: folderRow.last ? i18nc("@info:tooltip", "The last folder stays. To allow changes everywhere, turn this off.")
                                                      : i18nc("@info:tooltip", "Remove")
                    QQC2.ToolTip.visible: removeHover.hovered
                    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

                    QQC2.Button {
                        id: removeButton
                        objectName: "removeTrialFolder"
                        text: i18nc("@action:button", "Remove")
                        icon.name: "list-remove"
                        display: QQC2.AbstractButton.IconOnly
                        enabled: !folderRow.last && !page.account.writeScopeChanging
                        onClicked: page.account.removeFolder(folderRow.modelData)
                    }
                }
            }
        }
        FormCard.FormButtonDelegate {
            objectName: "addTrialFolder"
            visible: trialSwitch.visible && trialSwitch.on
            enabled: page.account !== null && !page.account.writeScopeChanging
            text: i18nc("@action:button", "Add Folder…")
            icon.name: "list-add"
            onClicked: trialFolderDialog.openFor(page.account, page.sync)
        }
    }

    // The folder: where the OneDrive shows up on this device.
    FormCard.FormHeader {
        visible: page.hasFolder
        title: i18nc("@title:group", "Folder")
    }
    FormCard.FormCard {
        visible: page.hasFolder

        FormCard.FormTextDelegate {
            text: page.sync ? page.sync.rootPathText : ""
            description: i18n("Your OneDrive on this device")
            leading: Kirigami.Icon {
                source: "folder-cloud"
                implicitWidth: Kirigami.Units.iconSizes.medium
                implicitHeight: Kirigami.Units.iconSizes.medium
            }
        }
        // A folder that stopped is given another on Status, by its own Use Another Folder….
        FormCard.FormButtonDelegate {
            objectName: "anotherFolderButton"
            visible: page.sync !== null && page.sync.overallReason !== "stopped"
            text: i18nc("@action:button", "Use Another Folder…")
            description: i18n("Your OneDrive moves to another empty folder; this one keeps its files as plain files.")
            icon.name: "folder-open"
            onClicked: anotherFolderDialog.open()
        }
    }

    // What the folder shows and keeps on this device: the account's own sync settings — its
    // previews (issue #80) and the free space its downloads leave (FreeSpaceReserve, issue
    // #217) — the daemon's, written to config.toml; each control shows what it says, and a
    // change goes through it. Pausing on metered connections and on battery is the whole
    // app's, on the Settings page (issue #95).
    FormCard.FormHeader {
        visible: page.hasFolder
        title: i18nc("@title:group", "On This Device")
    }
    FormCard.FormCard {
        objectName: "deviceSettings"
        visible: page.hasFolder

        FormCard.FormSwitchDelegate {
            id: thumbnailsSwitch
            objectName: "thumbnailsSwitch"

            readonly property bool on: page.sync !== null && page.sync.thumbnails

            text: i18n("Previews without downloading")
            description: i18n("Dolphin shows previews of images, videos, PDFs and office documents from the small thumbnails OneDrive makes.")
            checked: on
            // The inner switch writes `checked` back, ending the binding: kept in step here.
            onOnChanged: checked = on
            onToggled: {
                const wanted = checked;
                checked = on;
                page.sync.setThumbnails(wanted);
            }
        }
        // With the download guard on, the thumbnailer is refused: a file only in OneDrive shows
        // no preview rather than being downloaded for one.
        FormCard.FormTextDelegate {
            objectName: "thumbnailsOffLine"
            visible: !thumbnailsSwitch.on && !(DownloadGuard.serviceAvailable && DownloadGuard.enabled)
            text: i18n("While Dolphin shows previews, it downloads a file that is only in OneDrive in full to make its preview.")
            textItem.wrapMode: Text.Wrap
            leading: Kirigami.Icon {
                source: "dialog-warning"
                implicitWidth: Kirigami.Units.iconSizes.small
                implicitHeight: Kirigami.Units.iconSizes.small
            }
        }
        FormCard.FormDelegateSeparator {}
        // The box takes the wheel only after Edit, which turns into Save: a spin box turns with
        // the wheel, so scrolling the page over it would change the number. A disabled box
        // takes no input, and the page scrolls.
        FormCard.AbstractFormDelegate {
            id: freeSpaceReserveRow
            objectName: "freeSpaceReserveRow"

            /// The number is being changed: the box takes input, and the button saves it.
            property bool editing: false
            readonly property int kept: page.sync !== null ? page.sync.freeSpaceReserve : 0

            /// The number in the box, also one typed and not yet taken by it; within its range.
            function chosen() {
                try {
                    const typed = Math.round(reserveBox.valueFromText(reserveBox.contentItem.text, reserveBox.locale));
                    if (!isNaN(typed)) {
                        return Math.max(reserveBox.from, Math.min(reserveBox.to, typed));
                    }
                } catch (error) {
                    // Not a number: what the box holds.
                }
                return reserveBox.value;
            }

            function edit() {
                editing = true;
                reserveBox.contentItem.selectAll();
                reserveBox.forceActiveFocus();
            }

            /// Sends the number when it differs; the box then shows what the daemon keeps.
            function save() {
                const wanted = chosen();
                editing = false;
                if (page.sync !== null && wanted !== kept) {
                    page.sync.setFreeSpaceReserve(wanted);
                }
            }

            onKeptChanged: {
                if (!editing) {
                    reserveBox.value = kept;
                }
            }
            onEditingChanged: {
                if (!editing) {
                    reserveBox.value = kept;
                }
            }

            // Another account shown: its own number, not the one being typed.
            Connections {
                target: Current
                function onChanged() {
                    freeSpaceReserveRow.editing = false;
                }
            }

            focusPolicy: Qt.NoFocus
            background: null
            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing
                QQC2.Label {
                    Layout.fillWidth: true
                    text: i18n("Keep at least this much free space on this device, in GiB")
                    wrapMode: Text.Wrap
                    Accessible.ignored: true
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    QQC2.SpinBox {
                        id: reserveBox
                        objectName: "freeSpaceReserveBox"
                        Layout.fillWidth: true
                        from: 0
                        to: 100000
                        editable: true
                        enabled: freeSpaceReserveRow.editing
                        value: freeSpaceReserveRow.kept
                        Accessible.name: i18n("Keep at least this much free space on this device, in GiB")
                        Keys.onReturnPressed: freeSpaceReserveRow.save()
                        Keys.onEnterPressed: freeSpaceReserveRow.save()
                    }
                    QQC2.Button {
                        objectName: "editFreeSpaceReserveButton"
                        enabled: page.sync !== null
                        text: freeSpaceReserveRow.editing ? i18nc("@action:button", "Save") : i18nc("@action:button", "Edit")
                        icon.name: freeSpaceReserveRow.editing ? "document-save" : "document-edit"
                        onClicked: freeSpaceReserveRow.editing ? freeSpaceReserveRow.save() : freeSpaceReserveRow.edit()
                    }
                }
            }
        }
        FormCard.FormTextDelegate {
            objectName: "freeSpaceReserveLine"
            text: i18n("A file always kept on this device is not downloaded while its download would leave less free space than this.")
            textItem.wrapMode: Text.Wrap
        }
    }

    // What is seldom changed.
    FormCard.FormHeader {
        visible: page.available
        title: i18nc("@title:group", "Advanced")
    }
    FormCard.FormCard {
        objectName: "advancedSettings"
        visible: page.available

        FormCard.FormTextDelegate {
            visible: page.hasFolder
            text: i18n("Never uploaded")
            description: i18n("Files of your own whose names match one of these stay on this device and are never uploaded. A pattern matches a name, not a path: * stands for anything, ? for one character. Files that came from OneDrive are not affected.")
            textItem.wrapMode: Text.Wrap
        }
        Repeater {
            model: page.hasFolder && page.sync ? page.sync.ignorePatterns : []
            delegate: FormCard.FormTextDelegate {
                required property string modelData
                text: modelData
                trailing: QQC2.Button {
                    text: i18nc("@action:button", "Remove")
                    icon.name: "list-remove"
                    display: QQC2.AbstractButton.IconOnly
                    QQC2.ToolTip.text: text
                    QQC2.ToolTip.visible: hovered
                    onClicked: page.sync.removeIgnorePattern(modelData)
                }
            }
        }
        FormCard.FormTextFieldDelegate {
            id: ignoreField
            objectName: "ignoreField"
            visible: page.hasFolder
            label: i18n("Add a pattern")
            placeholderText: "*.tmp"
            onAccepted: addPattern.clicked()
        }
        FormCard.FormButtonDelegate {
            id: addPattern
            objectName: "addIgnorePattern"
            visible: page.hasFolder
            text: i18nc("@action:button", "Add")
            icon.name: "list-add"
            enabled: ignoreField.text.trim().length > 0
            onClicked: {
                page.sync.addIgnorePattern(ignoreField.text);
                ignoreField.text = "";
            }
        }
        FormCard.FormDelegateSeparator {
            visible: page.hasFolder && page.signedIn
        }
        FormCard.FormButtonDelegate {
            objectName: "signOutButton"
            visible: page.signedIn
            text: i18nc("@action:button", "Sign Out")
            description: i18n("The folder stays with the files already downloaded, but nothing syncs until you sign in again.")
            icon.name: "system-log-out"
            onClicked: page.account.signOut()
        }
    }

    FormCard.FormCard {
        Layout.topMargin: Kirigami.Units.largeSpacing * 2
        visible: page.available

        FormCard.FormButtonDelegate {
            objectName: "removeAccountButton"
            text: i18nc("@action:button", "Disconnect This OneDrive…")
            description: i18n("KOneDrive stops syncing this folder and signs out of the account. Nothing in OneDrive is deleted.")
            icon.name: "list-remove-user"
            // One Remove at a time: it forgets the folder first, which can take a while.
            enabled: Daemon.removing.length === 0
            onClicked: removeDialog.open()
        }
        FormCard.AbstractFormDelegate {
            objectName: "removingRow"
            visible: page.account !== null && Daemon.removing === page.account.path
            background: null
            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing
                QQC2.BusyIndicator {
                    running: parent.visible
                    implicitWidth: Kirigami.Units.iconSizes.medium
                    implicitHeight: Kirigami.Units.iconSizes.medium
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    text: i18n("Disconnecting…")
                }
            }
        }
    }

    Kirigami.PromptDialog {
        id: renameDialog
        objectName: "renameDialog"

        /// The account it was opened for (an AccountItem); null once that account is gone.
        property QtObject item: null
        readonly property string problem: item ? Accounts.labelProblem(labelField.text, item.path) : ""
        readonly property bool ready: item !== null && problem.length === 0 && labelField.text.trim() !== item.account.label

        function rename() {
            if (ready) {
                item.account.setLabel(labelField.text);
                close();
            }
        }

        title: i18nc("@title:window", "Rename")
        subtitle: i18n("The name KOneDrive shows for this OneDrive, in the sidebar, the tray and Places.")
        standardButtons: Kirigami.Dialog.NoButton
        onAboutToShow: item = Current.item
        onOpened: {
            labelField.text = item ? item.account.label : "";
            labelField.selectAll();
            labelField.forceActiveFocus();
        }

        // Another account shown, or this one gone: not the question asked.
        Connections {
            target: Current
            function onChanged() {
                renameDialog.close();
            }
        }

        QQC2.TextField {
            id: labelField
            Layout.fillWidth: true
            onAccepted: renameDialog.rename()
        }
        QQC2.Label {
            Layout.fillWidth: true
            visible: labelField.text.trim().length > 0 && renameDialog.problem.length > 0
            text: renameDialog.problem
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.negativeTextColor
        }

        customFooterActions: [
            Kirigami.Action {
                text: i18nc("@action:button", "Rename")
                icon.name: "edit-rename"
                enabled: renameDialog.ready
                onTriggered: renameDialog.rename()
            },
            Kirigami.Action {
                text: i18nc("@action:button", "Cancel")
                icon.name: "dialog-cancel"
                onTriggered: renameDialog.close()
            }
        ]
    }

    // A folder for the trial, picked in the OneDrive folder: one outside it, or the folder
    // itself, opens the dialog again in the OneDrive folder, headed by why.
    FolderDialog {
        id: trialFolderDialog
        objectName: "trialFolderDialog"

        /// The account and its folder's sync it is open for; null once closed.
        property QtObject account: null
        property QtObject sync: null

        function openFor(forAccount, forSync) {
            account = forAccount;
            sync = forSync;
            title = i18nc("@title:window", "Choose a Folder That Can Be Changed");
            currentFolder = forSync.rootUrl();
            open();
        }

        /// The folder picked: allowed, or the dialog opened again when it is not in the OneDrive.
        function pick(folder) {
            const relative = sync ? sync.folderInside(folder) : "";
            if (account && relative.length === 0) {
                title = i18nc("@title:window", "Choose a Folder Inside Your OneDrive Folder");
                currentFolder = sync.rootUrl();
                // Not while the dialog that answered is still closing.
                Qt.callLater(open);
                return;
            }
            if (account) {
                account.allowFolder(relative);
            }
            account = null;
            sync = null;
        }

        onAccepted: pick(selectedFolder)
        onRejected: {
            account = null;
            sync = null;
        }

        // Another account shown: not the question asked.
        Connections {
            target: Current
            function onChanged() {
                trialFolderDialog.account = null;
                trialFolderDialog.sync = null;
                trialFolderDialog.close();
            }
        }
    }

    // Ending the trial: every folder can be changed, and the list is forgotten.
    Kirigami.PromptDialog {
        id: allowEverywhereDialog
        objectName: "allowEverywhereDialog"

        title: i18nc("@title:window", "Allow Changes Everywhere?")
        maximumWidth: Kirigami.Units.gridUnit * 25
        subtitle: {
            const text = i18n("Every folder in this OneDrive can then be changed from this computer, and the list of folders is forgotten.");
            return page.holdOn ? text + " " + i18n("Large changes are still held for your review.") : text;
        }
        standardButtons: Kirigami.Dialog.NoButton
        customFooterActions: [
            Kirigami.Action {
                objectName: "allowEverywhere"
                text: i18nc("@action:button", "Allow Everywhere")
                icon.name: "object-unlocked"
                onTriggered: {
                    page.account.clearWriteScope();
                    allowEverywhereDialog.close();
                }
            },
            Kirigami.Action {
                text: i18nc("@action:button", "Cancel")
                icon.name: "dialog-cancel"
                onTriggered: allowEverywhereDialog.close()
            }
        ]

        // Another account shown, or the trial ended elsewhere: not the question now.
        Connections {
            target: Current
            function onChanged() {
                allowEverywhereDialog.close();
            }
        }
        Connections {
            target: page.account
            function onAccountChanged() {
                if (!trialSwitch.on) {
                    allowEverywhereDialog.close();
                }
            }
        }
    }

    AnotherFolderDialog {
        id: anotherFolderDialog
        objectName: "anotherFolderDialog"
        onConfirmed: page.window.showPage("status")
    }

    DisconnectDialog {
        id: removeDialog
        objectName: "removeDialog"
    }
}
