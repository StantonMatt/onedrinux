#pragma once

#include <QString>

/// Where a click on a notification opens the window: Status, its History, the changes on
/// hold, a section of Issues, the apps the download guard blocked in Settings, or a trial's
/// folders in OneDrive Settings.
enum class WindowSection {
    Status,
    /// Status with its History opened: what happened to the files.
    History,
    /// On Hold: the changes the change guard holds for review.
    OnHold,
    /// Issues: the changes that can't upload.
    CantUpload,
    /// Issues: the files whose both versions were kept.
    KeptBothVersions,
    /// Issues: what can't be shown in the folder.
    NotInFolder,
    /// Settings: the apps the download guard blocked or has a rule for.
    GuardApps,
    /// OneDrive Settings: the Sync card, with a trial's folders that can be changed.
    WriteScope,
};

/// `section` by the name the window's showSection() takes.
inline QString sectionName(WindowSection section)
{
    switch (section) {
    case WindowSection::Status:
        return QStringLiteral("status");
    case WindowSection::History:
        return QStringLiteral("history");
    case WindowSection::OnHold:
        return QStringLiteral("onHold");
    case WindowSection::CantUpload:
        return QStringLiteral("cantUpload");
    case WindowSection::KeptBothVersions:
        return QStringLiteral("keptBothVersions");
    case WindowSection::NotInFolder:
        return QStringLiteral("notInFolder");
    case WindowSection::GuardApps:
        return QStringLiteral("guardApps");
    case WindowSection::WriteScope:
        return QStringLiteral("writeScope");
    }
    return QStringLiteral("status");
}
