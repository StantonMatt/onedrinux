import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.kirigamiaddons.formcard as FormCard
import org.kde.kitemmodels as KItemModels
import org.konedrive.app

/// The changes the change guard holds for the OneDrive chosen in the sidebar, for review: what
/// the page's decisions did, the head with Allow All and Restore All when several kinds are
/// held, then one card per held kind (deletions, changed files, moves and renames, new files)
/// with the kind's two buttons, Restore first, and what of a held kind was sent before the
/// hold. A kind's card opens to its items, one group at a time, with a search when there are
/// many. Each button names what it does and how many it acts on, whatever the search shows;
/// Allow All asks first while deletions are among the kinds. Allow and Restore act on what the
/// page shows. A change a decision left on hold says why on its row.
///
/// One list: what is above the open group's items is its header, the items are its rows,
/// built only while the group is open and only those in sight, and what comes after is its
/// footer.
ListPage {
    id: page

    readonly property var held: Current.item ? Current.item.held : null
    readonly property var sync: Current.sync
    readonly property bool available: Daemon.serviceAvailable && held !== null && held.serviceAvailable && held.listingError.length === 0
    /// Deletions are among several held kinds: Allow All asks first, since it deletes in OneDrive.
    readonly property bool allowAllAsks: held !== null && held.kinds.length > 1 && held.kinds.some(kind => kind.kind === "removal")

    function escaped(text: string): string {
        return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    }
    /// The open group: a kind, held or sent before the hold; "" when every group is folded.
    property string openKind: ""
    property bool openSent: false
    /// What the open group's search keeps: items whose path holds it, in any case.
    property string searchText: ""
    readonly property var window: QQC2.ApplicationWindow.window

    /// Everything after the head, in order: each held kind's card, then Sent Before the Hold's
    /// header, its kinds and the recycle bin. Each {type, kind, data}.
    readonly property var blocks: {
        const list = [];
        if (held === null) {
            return list;
        }
        for (const kind of held.kinds) {
            list.push({ type: "held", kind: kind.kind, data: kind });
        }
        const sent = held.sentKinds;
        if (sent.length > 0) {
            list.push({ type: "sentHeader", kind: "", data: null });
            for (const kind of sent) {
                list.push({ type: "sent", kind: kind.kind, data: kind });
            }
            if (held.removalsSent) {
                list.push({ type: "recycleBin", kind: "", data: null });
            }
        }
        return list;
    }
    readonly property int openIndex: blocks.findIndex(block => block.type === (openSent ? "sent" : "held") && block.kind === openKind)
    /// The open group's block, or null.
    readonly property var openBlock: openIndex >= 0 ? blocks[openIndex] : null

    /// Opens a group, or folds it when it is the open one; one at a time.
    function toggle(kind, sent) {
        const open = !(openKind === kind && openSent === sent);
        openKind = open ? kind : "";
        openSent = sent;
        searchText = "";
        if (held) {
            held.showRows(openKind, openSent);
        }
    }

    objectName: "onHoldPage"
    title: window ? window.accountTitle(i18nc("@title", "On Hold")) : i18nc("@title", "On Hold")

    onVisibleChanged: {
        if (held) {
            held.setReviewing(visible);
        }
        // Left: every group folds.
        if (!visible && openKind.length > 0) {
            toggle(openKind, openSent);
        }
    }
    onHeldChanged: {
        openKind = "";
        searchText = "";
        sendAllDialog.close();
    }
    // The open group resolved: nothing to open, once the bindings have settled.
    onOpenIndexChanged: {
        if (openIndex < 0 && openKind.length > 0) {
            Qt.callLater(() => {
                if (openIndex < 0 && openKind.length > 0) {
                    openKind = "";
                    searchText = "";
                    if (held) {
                        held.showRows("", false);
                    }
                }
            });
        }
    }

    ListView {
        id: list

        /// The open group's items, those the search keeps.
        readonly property KItemModels.KSortFilterProxyModel items: KItemModels.KSortFilterProxyModel {
            sourceModel: page.held ? page.held.rows : null
            filterRoleName: "path"
            filterString: page.searchText
            filterCaseSensitivity: Qt.CaseInsensitive
        }

        objectName: "onHoldList"
        reuseItems: true
        model: page.openBlock !== null ? items : null

        header: ColumnLayout {
            width: ListView.view.width
            spacing: 0

            ServiceCard {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.largeSpacing
                visible: !Daemon.serviceAvailable
            }

            Kirigami.InlineMessage {
                objectName: "heldListingError"
                Layout.fillWidth: true
                Layout.maximumWidth: Kirigami.Units.gridUnit * 30
                Layout.alignment: Qt.AlignHCenter
                Layout.topMargin: Kirigami.Units.largeSpacing
                Layout.leftMargin: Kirigami.Units.largeSpacing
                Layout.rightMargin: Kirigami.Units.largeSpacing
                visible: page.held !== null && page.held.listingError.length > 0
                type: Kirigami.MessageType.Error
                // The daemon's words, in a sentence of the page's.
                text: page.held ? page.escaped(i18n("Could not read the changes on hold: %1. What is shown may be out of date.", page.held.listingError.replace(/\.$/, ""))) : ""
                actions: Kirigami.Action {
                    objectName: "retryHeldListing"
                    text: i18nc("@action:button", "Retry")
                    icon.name: "view-refresh"
                    enabled: Daemon.serviceAvailable
                    onTriggered: page.held.retry()
                }
            }

            // What the page's decisions did, newest first, until each is closed.
            Repeater {
                model: page.held ? page.held.results : []
                delegate: Kirigami.InlineMessage {
                    required property var modelData

                    objectName: "heldResult"
                    Layout.fillWidth: true
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 30
                    Layout.alignment: Qt.AlignHCenter
                    Layout.topMargin: Kirigami.Units.largeSpacing
                    Layout.leftMargin: Kirigami.Units.largeSpacing
                    Layout.rightMargin: Kirigami.Units.largeSpacing
                    visible: true
                    type: modelData.type === "positive" ? Kirigami.MessageType.Positive
                        : modelData.type === "warning" ? Kirigami.MessageType.Warning
                        : modelData.type === "error" ? Kirigami.MessageType.Error
                        : Kirigami.MessageType.Information
                    text: page.escaped(modelData.text)
                    showCloseButton: true
                    // Closed (not the page left): it goes.
                    onVisibleChanged: {
                        if (!visible && page.visible && page.held) {
                            page.held.closeResult(modelData.id);
                        }
                    }
                }
            }

            // The head: how much is held, since when, and the two buttons for every kind.
            FormCard.FormCard {
                objectName: "heldHeadCard"
                Layout.topMargin: Kirigami.Units.largeSpacing
                visible: page.held !== null && page.held.count > 0

                FormCard.AbstractFormDelegate {
                    background: null
                    focusPolicy: Qt.NoFocus
                    contentItem: RowLayout {
                        spacing: Kirigami.Units.largeSpacing

                        Kirigami.Icon {
                            Layout.alignment: Qt.AlignTop
                            source: "security-medium"
                            implicitWidth: Kirigami.Units.iconSizes.medium
                            implicitHeight: Kirigami.Units.iconSizes.medium
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: Kirigami.Units.smallSpacing

                            Kirigami.Heading {
                                objectName: "heldHeadline"
                                Layout.fillWidth: true
                                level: 3
                                text: page.held ? page.held.headline : ""
                                wrapMode: Text.Wrap
                            }
                            QQC2.Label {
                                Layout.fillWidth: true
                                text: page.held ? page.held.headDetail : ""
                                wrapMode: Text.Wrap
                                opacity: 0.8
                            }
                            Flow {
                                Layout.fillWidth: true
                                Layout.topMargin: Kirigami.Units.smallSpacing
                                spacing: Kirigami.Units.smallSpacing
                                visible: page.held !== null && page.held.kinds.length > 1

                                QQC2.Button {
                                    objectName: "restoreAllButton"
                                    text: i18nc("@action:button", "Restore All")
                                    icon.name: "edit-undo"
                                    enabled: page.available && !page.held.working
                                    onClicked: page.held.restore("")
                                }
                                QQC2.Button {
                                    objectName: "allowAllButton"
                                    text: i18nc("@action:button", "Allow All")
                                    icon.name: "dialog-ok-apply"
                                    enabled: page.available && !page.held.working
                                    onClicked: {
                                        if (page.allowAllAsks) {
                                            sendAllDialog.question = page.held.sendAllQuestion();
                                            sendAllDialog.open();
                                        } else {
                                            page.held.allow("");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Nothing on hold: the page stays, with what was done, until another is shown.
            FormCard.FormCard {
                objectName: "nothingHeldCard"
                Layout.topMargin: Kirigami.Units.largeSpacing
                visible: page.available && page.held.count === 0

                FormCard.FormPlaceholderMessageDelegate {
                    text: i18n("Nothing on hold")
                    icon.name: "security-high"
                }
            }

            Blocks {
                Layout.fillWidth: true
                blocks: page.openIndex < 0 ? page.blocks : page.blocks.slice(0, page.openIndex + 1)
                openEnds: page.openIndex >= 0
            }
        }

        delegate: CardBand {
            id: row

            required property int index
            required property string name
            required property string path
            required property string detail
            required property string detailToolTip
            required property string iconName
            required property string sizeText
            required property string afterText
            required property bool emptied
            required property string countText
            required property string localPath
            required property string failure

            objectName: "heldRow"
            implicitHeight: entry.implicitHeight

            FormCard.FormDelegateSeparator {
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: Kirigami.Units.largeSpacing
                anchors.rightMargin: Kirigami.Units.largeSpacing
                z: 1
            }
            FormCard.AbstractFormDelegate {
                id: entry

                anchors.left: parent.left
                anchors.right: parent.right
                // A deleted item isn't in the folder to show: its row does nothing.
                hoverEnabled: row.localPath.length > 0
                focusPolicy: row.localPath.length > 0 ? Qt.StrongFocus : Qt.NoFocus
                topPadding: Kirigami.Units.smallSpacing + Kirigami.Units.mediumSpacing
                bottomPadding: Kirigami.Units.smallSpacing + Kirigami.Units.mediumSpacing
                Accessible.name: row.name
                Accessible.description: row.failure.length > 0 ? row.failure : row.detail
                onClicked: {
                    if (row.localPath.length > 0) {
                        page.sync.showInFolder(row.localPath);
                    }
                }

                contentItem: RowLayout {
                    spacing: Kirigami.Units.largeSpacing

                    Kirigami.Icon {
                        source: row.iconName
                        fallback: "unknown"
                        implicitWidth: Kirigami.Units.iconSizes.smallMedium
                        implicitHeight: Kirigami.Units.iconSizes.smallMedium
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0

                        QQC2.Label {
                            Layout.fillWidth: true
                            text: row.name
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                        }
                        // Its folder; left on hold by a decision, why, with the path on hover.
                        QQC2.Label {
                            id: detailLabel
                            objectName: "heldRowDetail"
                            Layout.fillWidth: true
                            text: row.failure.length > 0 ? row.failure : row.detail
                            textFormat: Text.PlainText
                            elide: row.failure.length > 0 ? Text.ElideRight : Text.ElideMiddle
                            color: row.failure.length > 0 ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.disabledTextColor
                            font: Kirigami.Theme.smallFont

                            HoverHandler {
                                id: detailHover
                            }
                            QQC2.ToolTip.text: page.escaped(row.detailToolTip)
                            QQC2.ToolTip.visible: detailHover.hovered && (detailLabel.truncated || row.failure.length > 0)
                            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        }
                    }
                    ColumnLayout {
                        Layout.alignment: Qt.AlignVCenter
                        spacing: 0

                        // A changed file's size before and after; emptied, the after stands out.
                        QQC2.Label {
                            Layout.alignment: Qt.AlignRight
                            visible: row.sizeText.length > 0
                            text: row.afterText.length === 0 ? row.sizeText
                                : row.emptied ? i18nc("@info a changed file's size before, then after", "%1 → %2", row.sizeText,
                                                      "<b><font color=\"" + Kirigami.Theme.negativeTextColor + "\">" + row.afterText + "</font></b>")
                                : i18nc("@info a changed file's size before, then after", "%1 → %2", row.sizeText, row.afterText)
                            textFormat: row.emptied ? Text.StyledText : Text.PlainText
                            color: Kirigami.Theme.disabledTextColor
                            font: Kirigami.Theme.smallFont
                        }
                        QQC2.Label {
                            Layout.alignment: Qt.AlignRight
                            visible: text.length > 0
                            text: row.countText
                            color: Kirigami.Theme.disabledTextColor
                            font: Kirigami.Theme.smallFont
                        }
                    }
                }
            }
        }

        footer: ColumnLayout {
            width: ListView.view.width
            spacing: 0

            // The open group's search found nothing; it does not look into a folder its row
            // stands for.
            CardBand {
                Layout.fillWidth: true
                implicitHeight: nothingFound.implicitHeight
                visible: page.openBlock !== null && page.searchText.length > 0 && list.count === 0

                FormCard.FormPlaceholderMessageDelegate {
                    id: nothingFound
                    objectName: "nothingMatches"
                    anchors.left: parent.left
                    anchors.right: parent.right
                    text: i18n("Nothing matches “%1”", page.searchText)
                    explanation: {
                        if (!page.held || !page.held.rows.folders) {
                            return "";
                        }
                        if (page.openKind === "removal") {
                            return i18n("What is in the deleted folders isn't searched.");
                        }
                        if (page.openKind === "move") {
                            return i18n("What is in the moved folders isn't searched.");
                        }
                        return i18n("What is in the new folders isn't searched.");
                    }
                    icon.name: "edit-find"
                }
            }
            // The open group's card ends under its items.
            CardEnd {
                visible: page.openBlock !== null
            }
            Blocks {
                Layout.fillWidth: true
                blocks: page.openIndex < 0 ? [] : page.blocks.slice(page.openIndex + 1)
                openEnds: false
            }
            Item {
                implicitHeight: Kirigami.Units.largeSpacing * 2
            }
        }
    }

    // Allow All while deletions are held: what it sends, deletions first, before it goes. It
    // says and sends what was held when it opened, whatever is held after.
    Kirigami.PromptDialog {
        id: sendAllDialog
        objectName: "sendAllDialog"

        /// HeldChanges.sendAllQuestion(), as it opened.
        property var question: null

        title: question ? question.title : ""
        subtitle: question ? question.text : ""
        maximumWidth: Kirigami.Units.gridUnit * 25
        standardButtons: Kirigami.Dialog.NoButton
        customFooterActions: [
            Kirigami.Action {
                objectName: "sendAll"
                text: i18nc("@action:button", "Send All")
                icon.name: "cloud-upload"
                enabled: page.available && !page.held.working
                onTriggered: {
                    page.held.allowAsked(sendAllDialog.question);
                    sendAllDialog.close();
                }
            },
            Kirigami.Action {
                text: i18nc("@action:button", "Cancel")
                icon.name: "dialog-cancel"
                onTriggered: sendAllDialog.close()
            }
        ]

        // Left, or no longer the question: the deletions or the other kinds resolved meanwhile.
        Connections {
            target: page
            function onAllowAllAsksChanged() {
                if (!page.allowAllAsks) {
                    sendAllDialog.close();
                }
            }
            function onVisibleChanged() {
                if (!page.visible) {
                    sendAllDialog.close();
                }
            }
        }
    }

    /// A row of the open group's card, which its items go on: a FormCardRow with the card's
    /// sides where the card is narrower than the page.
    component CardBand: FormCardRow {
        id: band

        /// As FormCard draws its border.
        readonly property color borderColor: {
            const background = cardColors.Kirigami.Theme.backgroundColor;
            const dark = 1 - (0.299 * background.r + 0.587 * background.g + 0.114 * background.b) >= 0.4;
            return dark ? Qt.darker(background, 1.2) : Kirigami.ColorUtils.linearInterpolation(background, cardColors.Kirigami.Theme.textColor, 0.15);
        }
        /// The card has sides: the page is wider than it.
        readonly property bool sided: width > Kirigami.Units.gridUnit * 30

        Item {
            id: cardColors
            Kirigami.Theme.colorSet: Kirigami.Theme.View
            Kirigami.Theme.inherit: false
        }
        // On the card-coloured band, which is the card's width.
        Rectangle {
            visible: band.sided
            anchors.left: parent.left
            width: 1
            height: parent.height
            color: band.borderColor
            z: 2
        }
        Rectangle {
            visible: band.sided
            anchors.right: parent.right
            width: 1
            height: parent.height
            color: band.borderColor
            z: 2
        }
    }

    /// The bottom of the open group's card, under its items: its rounded end.
    component CardEnd: Item {
        id: end

        readonly property bool sided: width > Kirigami.Units.gridUnit * 30

        Layout.fillWidth: true
        implicitHeight: Kirigami.Units.largeSpacing

        Item {
            id: endColors
            Kirigami.Theme.colorSet: Kirigami.Theme.View
            Kirigami.Theme.inherit: false
        }
        Kirigami.ShadowedRectangle {
            readonly property color background: endColors.Kirigami.Theme.backgroundColor
            readonly property bool dark: 1 - (0.299 * background.r + 0.587 * background.g + 0.114 * background.b) >= 0.4

            anchors.horizontalCenter: parent.horizontalCenter
            width: Math.min(parent.width, Kirigami.Units.gridUnit * 30)
            height: parent.height
            color: background
            corners.topLeftRadius: 0
            corners.topRightRadius: 0
            radius: end.sided ? Kirigami.Units.cornerRadius : 0
            border.width: end.sided ? 1 : 0
            border.color: dark ? Qt.darker(background, 1.2) : Kirigami.ColorUtils.linearInterpolation(background, endColors.Kirigami.Theme.textColor, 0.15)

            // The sides go on from the rows above: no line across the top.
            Rectangle {
                x: 1
                width: parent.width - 2
                height: 1
                color: parent.background
            }
        }
    }

    /// The groups after the head, as `blocks` lists them: each held kind's card, and Sent
    /// Before the Hold's header and card. With `openEnds`, the last is the open group, whose
    /// card the list's rows go on with.
    component Blocks: ColumnLayout {
        id: blocksColumn

        required property var blocks
        required property bool openEnds
        readonly property var sentRows: blocks.filter(block => block.type === "sent" || block.type === "recycleBin")

        spacing: 0

        Repeater {
            model: blocksColumn.blocks.filter(block => block.type === "held")
            delegate: GroupCard {
                required property var modelData
                required property int index

                Layout.topMargin: Kirigami.Units.largeSpacing
                group: modelData.data
                open: page.openIndex >= 0 && !page.openSent && page.openKind === modelData.kind
            }
        }
        FormCard.FormHeader {
            visible: blocksColumn.blocks.some(block => block.type === "sentHeader")
            title: i18nc("@title:group", "Sent Before the Hold")
        }
        FormCard.FormCard {
            objectName: "sentCard"
            Layout.topMargin: blocksColumn.blocks.some(block => block.type === "sentHeader") ? 0 : -Kirigami.Units.smallSpacing
            visible: blocksColumn.sentRows.length > 0

            Repeater {
                model: blocksColumn.sentRows
                delegate: ColumnLayout {
                    id: sentRow

                    required property var modelData
                    required property int index
                    readonly property bool open: page.openIndex >= 0 && page.openSent && page.openKind === modelData.kind

                    Layout.fillWidth: true
                    spacing: 0

                    FormCard.FormDelegateSeparator {
                        visible: sentRow.index > 0
                    }
                    GroupHead {
                        visible: sentRow.modelData.type === "sent"
                        objectName: "sentToggle"
                        iconName: sentRow.modelData.data ? sentRow.modelData.data.iconName : ""
                        title: sentRow.modelData.data ? sentRow.modelData.data.title : ""
                        detail: sentRow.modelData.data ? sentRow.modelData.data.detail : ""
                        open: sentRow.open
                        onClicked: page.toggle(sentRow.modelData.kind, true)
                    }
                    FormCard.FormButtonDelegate {
                        objectName: "recycleBinButton"
                        visible: sentRow.modelData.type === "recycleBin"
                        text: i18nc("@action:button", "Open Recycle Bin")
                        description: i18n("OneDrive on the web")
                        icon.name: "internet-services"
                        onClicked: page.sync.openRecycleBin()
                    }
                }
            }
        }
    }

    /// A group's head: its kind's icon, the kind in bold, how much and since when, and the
    /// arrow that opens and folds it.
    component GroupHead: FormCard.AbstractFormDelegate {
        id: head

        property string iconName
        property string title
        property string detail
        property bool open: false

        Layout.fillWidth: true
        Accessible.name: title
        Accessible.description: detail

        contentItem: RowLayout {
            spacing: Kirigami.Units.largeSpacing

            Kirigami.Icon {
                Layout.alignment: Qt.AlignTop
                source: head.iconName
                implicitWidth: Kirigami.Units.iconSizes.smallMedium
                implicitHeight: Kirigami.Units.iconSizes.smallMedium
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0

                QQC2.Label {
                    Layout.fillWidth: true
                    text: head.title
                    font.bold: true
                    elide: Text.ElideRight
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    text: head.detail
                    wrapMode: Text.Wrap
                    color: Kirigami.Theme.disabledTextColor
                    font: Kirigami.Theme.smallFont
                }
            }
            FormCard.FormArrow {
                Layout.alignment: Qt.AlignVCenter
                direction: head.open ? Qt.UpArrow : Qt.DownArrow
            }
        }
    }

    /// One held kind: its head, a line where its outcome needs one, its two buttons (or how
    /// far a decision is), and, open, the search over its items.
    component GroupCard: FormCard.FormCard {
        id: card

        required property var group
        required property bool open
        readonly property int indent: Kirigami.Units.iconSizes.smallMedium + Kirigami.Units.largeSpacing

        objectName: "heldGroup-" + group.kind

        // Open, the card goes on below with its items: its lower corners are square.
        Binding {
            target: card.children.length > 0 && card.children[0].corners !== undefined ? card.children[0].corners : null
            property: "bottomLeftRadius"
            value: card.open ? 0 : -1
        }
        Binding {
            target: card.children.length > 0 && card.children[0].corners !== undefined ? card.children[0].corners : null
            property: "bottomRightRadius"
            value: card.open ? 0 : -1
        }

        GroupHead {
            objectName: "groupToggle"
            bottomPadding: Kirigami.Units.smallSpacing
            iconName: card.group.iconName
            title: card.group.title
            detail: card.group.detail
            open: card.open
            onClicked: page.toggle(card.group.kind, false)
        }
        FormCard.AbstractFormDelegate {
            background: null
            focusPolicy: Qt.NoFocus
            topPadding: 0
            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                QQC2.Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: card.indent
                    visible: text.length > 0
                    text: card.group.note
                    wrapMode: Text.Wrap
                    color: Kirigami.Theme.disabledTextColor
                    font: Kirigami.Theme.smallFont
                }
                Flow {
                    objectName: "groupButtons"
                    Layout.fillWidth: true
                    Layout.leftMargin: card.indent
                    spacing: Kirigami.Units.smallSpacing
                    visible: !card.group.working

                    // The undo first, as in the notification.
                    QQC2.Button {
                        objectName: "restoreButton"
                        text: card.group.restoreText
                        icon.name: card.group.restoreIcon
                        enabled: page.available
                        onClicked: page.held.restore(card.group.kind)
                    }
                    QQC2.Button {
                        objectName: "allowButton"
                        text: card.group.allowText
                        icon.name: card.group.allowIcon
                        enabled: page.available
                        onClicked: page.held.allow(card.group.kind)
                    }
                }
                // A decision under way: the buttons give way to how far it is.
                RowLayout {
                    objectName: "groupWorking"
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.largeSpacing
                    visible: card.group.working

                    QQC2.BusyIndicator {
                        running: parent.visible
                        implicitWidth: Kirigami.Units.iconSizes.smallMedium
                        implicitHeight: Kirigami.Units.iconSizes.smallMedium
                        padding: 0
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        QQC2.Label {
                            Layout.fillWidth: true
                            text: card.group.workingText
                            elide: Text.ElideRight
                        }
                        QQC2.ProgressBar {
                            objectName: "groupProgress"
                            Layout.fillWidth: true
                            from: 0
                            to: 1
                            value: Math.max(0, card.group.progress)
                            indeterminate: card.group.progress < 0
                        }
                    }
                }
            }
        }
        // Open: a search when there are many, and the items as the list's rows below.
        FormCard.AbstractFormDelegate {
            visible: card.open && page.held !== null && page.held.rows.count > 10
            background: null
            focusPolicy: Qt.NoFocus
            topPadding: 0
            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                FormCard.FormDelegateSeparator {
                    Layout.fillWidth: true
                }
                Kirigami.SearchField {
                    objectName: "heldSearch"
                    Layout.fillWidth: true
                    placeholderText: i18nc("@info:placeholder", "Search by name or folder…")
                    text: page.searchText
                    onTextEdited: page.searchText = text
                }
            }
        }
    }
}
