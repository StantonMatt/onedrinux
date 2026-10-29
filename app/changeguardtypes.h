#pragma once

// The structured types of konedrived's change guard (the QtTypeName annotations of
// dbus/org.konedrive.ChangeGuard.xml name them). qdbusxml2cpp takes one include, so this
// header carries them all.

#include <QDBusArgument>
#include <QDBusMetaType>
#include <QList>
#include <QMap>
#include <QMetaType>
#include <QString>

/// One row of ListHeld: a change on hold, or one of a held kind sent in the window before the
/// hold. A removed or new folder stands for what was in it (`descendants`, with their bytes).
struct KonedriveHeldChange {
    /// Negative for a cleanup the daemon made.
    qint64 item = 0;
    /// Relative to the drive root; for a move, the new path.
    QString path;
    /// removal, rewrite, move or upload.
    QString kind;
    /// The size before (for a new file, its size; for a folder, all in it).
    qulonglong bytes = 0;
    /// Unix seconds.
    qint64 since = 0;
    QString generation;
    /// held, restoring or sent.
    QString state;
    bool directory = false;
    /// The items in a folder, not counting it.
    qulonglong descendants = 0;
    /// The size after, for a changed file.
    qulonglong afterBytes = 0;
    /// The path before, for a move.
    QString oldPath;
    bool sent = false;
};
using KonedriveHeldChangeList = QList<KonedriveHeldChange>;
Q_DECLARE_METATYPE(KonedriveHeldChange)

inline QDBusArgument &operator<<(QDBusArgument &argument, const KonedriveHeldChange &row)
{
    argument.beginStructure();
    argument << row.item << row.path << row.kind << row.bytes << row.since << row.generation << row.state << row.directory << row.descendants
             << row.afterBytes << row.oldPath << row.sent;
    argument.endStructure();
    return argument;
}

inline const QDBusArgument &operator>>(const QDBusArgument &argument, KonedriveHeldChange &row)
{
    argument.beginStructure();
    argument >> row.item >> row.path >> row.kind >> row.bytes >> row.since >> row.generation >> row.state >> row.directory >> row.descendants
        >> row.afterBytes >> row.oldPath >> row.sent;
    argument.endStructure();
    return argument;
}

/// One change a decision could not make: (local item id, relative path, plain-text reason).
struct KonedriveGuardFailure {
    qint64 item = 0;
    QString path;
    QString reason;
};
Q_DECLARE_METATYPE(KonedriveGuardFailure)

inline QDBusArgument &operator<<(QDBusArgument &argument, const KonedriveGuardFailure &failure)
{
    argument.beginStructure();
    argument << failure.item << failure.path << failure.reason;
    argument.endStructure();
    return argument;
}

inline const QDBusArgument &operator>>(const QDBusArgument &argument, KonedriveGuardFailure &failure)
{
    argument.beginStructure();
    argument >> failure.item >> failure.path >> failure.reason;
    argument.endStructure();
    return argument;
}

/// What AllowHeld, RestoreHeld and UploadLocal did, counted in single changes (a folder's
/// items each count). Allow's `done` is released for sending, not yet in OneDrive.
struct KonedriveGuardResult {
    qulonglong done = 0;
    /// Released, but OneDrive could not be reached at the last try.
    qulonglong pendingOffline = 0;
    qulonglong failed = 0;
    /// Changed files restored with yours kept beside them as a copy.
    qulonglong keptBothVersions = 0;
    QList<KonedriveGuardFailure> failures;
};
Q_DECLARE_METATYPE(KonedriveGuardResult)

inline QDBusArgument &operator<<(QDBusArgument &argument, const KonedriveGuardResult &result)
{
    argument.beginStructure();
    argument << result.done << result.pendingOffline << result.failed << result.keptBothVersions << result.failures;
    argument.endStructure();
    return argument;
}

inline const QDBusArgument &operator>>(const QDBusArgument &argument, KonedriveGuardResult &result)
{
    argument.beginStructure();
    argument >> result.done >> result.pendingOffline >> result.failed >> result.keptBothVersions >> result.failures;
    argument.endStructure();
    return argument;
}

/// SetThresholds' and GetSettings' limits by key: removals, removal-bytes, daily-removals,
/// rewrites, rewrite-bytes, moves, uploads, upload-bytes, window-seconds, daily-window-seconds.
using KonedriveThresholds = QMap<QString, qulonglong>;

/// Registers the types with Qt D-Bus, once, before the first call that carries them.
inline void registerKonedriveChangeGuardTypes()
{
    qDBusRegisterMetaType<KonedriveHeldChange>();
    qDBusRegisterMetaType<KonedriveHeldChangeList>();
    qDBusRegisterMetaType<KonedriveGuardFailure>();
    qDBusRegisterMetaType<QList<KonedriveGuardFailure>>();
    qDBusRegisterMetaType<KonedriveGuardResult>();
    qDBusRegisterMetaType<KonedriveThresholds>();
    // By their names too: an adaptor's slots (the tests' fake daemon) are looked up by them.
    qRegisterMetaType<KonedriveHeldChangeList>("KonedriveHeldChangeList");
    qRegisterMetaType<KonedriveThresholds>("KonedriveThresholds");
}
