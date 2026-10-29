import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.kirigamiaddons.formcard as FormCard
import org.konedrive.app

/// One app the download guard blocked or has a rule for (a row of DownloadGuard.apps): its
/// icon, its name, what the guard does with it, and what can be done. A blocked app is allowed
/// for this session or always; from its menu it is always blocked, or what it downloaded is
/// freed up. A rule is removed, and the app is counted from zero again. A program that could
/// not be identified can't be allowed, and has nothing to press. The name is the program's
/// own say: shown as plain text, never as markup.
FormCard.AbstractFormDelegate {
    id: row

    /// identity, name, iconName, state and stateText.
    required property var app
    /// Whether Always Allow is offered here; a terminal or an IDE is always allowed only from
    /// Settings, so one click on Status can't let every program run in it download freely.
    property bool offersAlways: true

    readonly property bool blocked: app.state === "blocked"
    readonly property bool hasRule: app.state === "allowed-session" || app.state === "allowed-always" || app.state === "blocked-always"
    /// Too narrow for the buttons beside the name: they go below it.
    readonly property bool narrow: blocked && width < Kirigami.Units.gridUnit * 26

    objectName: "guardAppRow"
    focusPolicy: Qt.NoFocus
    background: null
    Accessible.name: app.name
    Accessible.description: app.stateText

    contentItem: GridLayout {
        columns: row.narrow ? 2 : 3
        columnSpacing: Kirigami.Units.largeSpacing
        rowSpacing: Kirigami.Units.smallSpacing

        Kirigami.Icon {
            id: icon
            Layout.alignment: Qt.AlignVCenter
            source: row.app.iconName
            fallback: "application-x-executable"
            implicitWidth: Kirigami.Units.iconSizes.medium
            implicitHeight: Kirigami.Units.iconSizes.medium
        }
        ColumnLayout {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            spacing: 0

            QQC2.Label {
                objectName: "guardAppName"
                Layout.fillWidth: true
                text: row.app.name
                textFormat: Text.PlainText
                elide: Text.ElideRight
                Accessible.ignored: true
            }
            QQC2.Label {
                objectName: "guardAppState"
                Layout.fillWidth: true
                text: row.app.stateText
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Kirigami.Theme.disabledTextColor
                Accessible.ignored: true
            }
        }
        RowLayout {
            Layout.columnSpan: row.narrow ? 2 : 1
            Layout.leftMargin: row.narrow ? icon.width + Kirigami.Units.largeSpacing : 0
            Layout.alignment: Qt.AlignVCenter
            spacing: Kirigami.Units.smallSpacing

            QQC2.Button {
                objectName: "allowSessionButton"
                visible: row.blocked
                text: i18nc("@action:button", "Allow for This Session")
                onClicked: DownloadGuard.allowApp(row.app.identity, false)
            }
            QQC2.Button {
                objectName: "allowAlwaysButton"
                visible: row.blocked && row.offersAlways
                text: i18nc("@action:button", "Always Allow")
                onClicked: DownloadGuard.allowApp(row.app.identity, true)
            }
            QQC2.ToolButton {
                objectName: "moreButton"
                visible: row.blocked && DownloadGuard.freeingUp !== row.app.identity
                text: i18nc("@action:button", "More")
                icon.name: "overflow-menu"
                display: QQC2.AbstractButton.IconOnly
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                onClicked: menu.popup(this, 0, height)
            }
            QQC2.BusyIndicator {
                objectName: "freeingUpIndicator"
                visible: row.blocked && DownloadGuard.freeingUp === row.app.identity
                running: visible
                implicitWidth: Kirigami.Units.iconSizes.smallMedium
                implicitHeight: Kirigami.Units.iconSizes.smallMedium
            }
            QQC2.Button {
                objectName: "removeButton"
                visible: row.hasRule
                text: i18nc("@action:button remove the app's rule", "Remove")
                icon.name: "list-remove"
                display: QQC2.AbstractButton.IconOnly
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                onClicked: DownloadGuard.resetApp(row.app.identity)
            }
        }
    }

    QQC2.Menu {
        id: menu

        QQC2.MenuItem {
            objectName: "alwaysBlockItem"
            text: i18nc("@action:inmenu", "Always Block")
            icon.name: "dialog-cancel"
            onTriggered: DownloadGuard.blockApp(row.app.identity)
        }
        QQC2.MenuItem {
            objectName: "freeUpItem"
            text: i18nc("@action:inmenu", "Free Up What It Downloaded")
            icon.name: "edit-clear"
            enabled: DownloadGuard.freeingUp.length === 0
            onTriggered: DownloadGuard.freeUpAppDownloads(row.app.identity)
        }
    }
}
