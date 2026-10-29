#pragma once

#include "changeguardtypes.h"

#include <QAbstractListModel>
#include <QDBusConnection>
#include <QHash>
#include <QList>
#include <QObject>
#include <QSet>
#include <QString>
#include <QStringList>
#include <QVariantList>

#include <functional>

class OrgKonedriveChangeGuardInterface;
class QDBusServiceWatcher;
class QTimer;

/// The rows of one kind on the On Hold page, the open group's: one per held (or sent) row as
/// ListHeld gives it, a folder standing for what was in it, sorted by path as Dolphin sorts
/// names. Built only for the open group; the page's list builds only the rows in sight.
class HeldRowsModel : public QAbstractListModel
{
    Q_OBJECT
    Q_PROPERTY(int count READ rowCount NOTIFY countChanged)
    /// A row is a folder standing for what was in it, which the search does not look into.
    Q_PROPERTY(bool folders READ folders NOTIFY foldersChanged)

public:
    enum Roles {
        NameRole = Qt::UserRole + 1,
        /// Relative to the drive root, the new one for a move: what the search matches.
        PathRole,
        /// The second line: the folder, or where a move came from.
        DetailRole,
        /// The full path of the second line, for its tooltip.
        DetailToolTipRole,
        IconNameRole,
        /// The right column's first line: a size, or "48 KiB" of "48 KiB → 0 B".
        SizeRole,
        /// "0 B" of "48 KiB → 0 B", for a changed file; else empty.
        AfterRole,
        /// The file was emptied: its after size stands out.
        EmptiedRole,
        /// The right column's second line: "186 items", for a folder.
        CountRole,
        /// The item in the folder, to show in Dolphin; empty for a deletion.
        LocalPathRole,
        /// Why the last decision left it on hold, as the daemon said it ("It is open in another
        /// program"); empty when none did.
        FailureRole,
    };
    Q_ENUM(Roles)

    explicit HeldRowsModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role = Qt::DisplayRole) const override;
    QHash<int, QByteArray> roleNames() const override;

    bool folders() const { return m_folders; }

    /// `rows`, which are all of one kind, sorted; `root` is the folder's path; `failures` the
    /// daemon's reasons by path, for the rows the last decision left on hold.
    void setRows(KonedriveHeldChangeList rows, const QString &root, const QHash<QString, QString> &failures = {});

Q_SIGNALS:
    void countChanged();
    void foldersChanged();

private:
    QString failure(const KonedriveHeldChange &row) const;

    KonedriveHeldChangeList m_rows;
    QString m_root;
    QHash<QString, QString> m_failures;
    bool m_folders = false;
};

/// What konedrived's change guard holds for one account (dbus/org.konedrive.ChangeGuard.xml):
/// the changes on hold by kind, with the token of what was listed, and what was sent of a held
/// kind in the window before the hold. Listed when the daemon comes and after each of its
/// signals (at most one listing at a time, the next after a short wait): the rows are what
/// the daemon says. Allow and Restore act on what was listed, through its token, so that what
/// was held after it was read is never decided unseen. Never blocks the GUI thread.
class HeldChanges : public QObject
{
    Q_OBJECT
    /// The daemon lists this account's holds; false while it is away, or has no change guard.
    Q_PROPERTY(bool serviceAvailable READ serviceAvailable NOTIFY changed)
    /// Why ListHeld failed; the last known holds stay until a successful retry.
    Q_PROPERTY(QString listingError READ listingError NOTIFY changed)
    /// The single changes on hold, a folder's items counted: the sidebar's badge.
    Q_PROPERTY(int count READ count NOTIFY changed)
    /// The held kinds in the page's order (deletions, changed files, moves, new files), each a
    /// map: kind, title ("Deletions"), detail ("312 items · 1.4 GiB · since 14:02"), countText
    /// ("312 deletions"), statusDetail ("1.4 GiB · since 14:02"), iconName, note, allowText,
    /// allowIcon, restoreText, restoreIcon, working, workingText, progress (0–1, or -1 while
    /// it is not known).
    Q_PROPERTY(QVariantList kinds READ kinds NOTIFY changed)
    /// The held kinds sent in the window before the hold, in the same order, each a map: kind,
    /// title ("20 deletions"), detail ("13:58–14:02 · in OneDrive's recycle bin"), iconName.
    Q_PROPERTY(QVariantList sentKinds READ sentKinds NOTIFY changed)
    /// Deletions were sent before the hold: the page offers OneDrive's recycle bin.
    Q_PROPERTY(bool removalsSent READ removalsSent NOTIFY changed)
    /// "312 deletions and 58 changed files are on hold".
    Q_PROPERTY(QString headline READ headline NOTIFY changed)
    /// "They were made in the folder since 14:02 and are not sent to OneDrive until you allow
    /// them. Everything else keeps syncing."
    Q_PROPERTY(QString headDetail READ headDetail NOTIFY changed)
    /// "312 deletions on hold", "370 changes on hold": the tray's and Status' words.
    Q_PROPERTY(QString summary READ summary NOTIFY changed)
    /// Allow All's question while deletions are among several held kinds: "Send All 370
    /// Changes to OneDrive?", and what it sends: "312 items will be deleted in OneDrive and 58
    /// changed files will be uploaded."
    Q_PROPERTY(QString sendAllTitle READ sendAllTitle NOTIFY changed)
    Q_PROPERTY(QString sendAllText READ sendAllText NOTIFY changed)
    /// The token of what is listed.
    Q_PROPERTY(QString token READ token NOTIFY changed)
    /// A decision is under way: Allow All and Restore All wait for it.
    Q_PROPERTY(bool working READ working NOTIFY changed)
    /// What the page's decisions did, newest first, each a map: id, type (positive,
    /// information, warning, error) and text.
    Q_PROPERTY(QVariantList results READ results NOTIFY resultsChanged)
    /// The open group's rows (showRows()).
    Q_PROPERTY(HeldRowsModel *rows READ rows CONSTANT)
    /// Why the last Upload asked from Issues was refused.
    Q_PROPERTY(QString actionError READ actionError NOTIFY actionErrorChanged)

public:
    static const QStringList KindOrder;
    /// Unix seconds.
    using Clock = std::function<qint64()>;

    /// One held kind, summed: what the notification says of it.
    struct Kind {
        QString kind;
        /// Single changes, a folder's items counted.
        qulonglong count = 0;
        qulonglong bytes = 0;
        qint64 since = 0;
    };

    /// The account at `accountPath` (its id is the last part).
    HeldChanges(const QDBusConnection &bus, const QString &accountPath, QObject *parent = nullptr);
    ~HeldChanges() override;

    bool serviceAvailable() const { return m_serviceAvailable; }
    QString listingError() const { return m_listingError; }
    int count() const;
    QVariantList kinds() const;
    QVariantList sentKinds() const;
    bool removalsSent() const;
    QString headline() const;
    QString headDetail() const;
    QString summary() const;
    QString sendAllTitle() const;
    QString sendAllText() const;
    bool working() const { return !m_calls.isEmpty(); }
    QVariantList results() const { return m_results; }
    HeldRowsModel *rows() const { return m_rows; }
    QString actionError() const { return m_actionError; }

    QString accountId() const { return m_id; }
    /// The token of what is listed.
    QString token() const { return m_token; }
    /// The held kinds as listed, in the page's order.
    QList<Kind> held() const;
    /// What of the held kinds was sent in the window before the hold, in the same order.
    QList<Kind> sent() const;
    /// The held rows as listed.
    KonedriveHeldChangeList heldRows() const { return m_held; }
    /// Whether the listing is the daemon's first answer since the app started.
    bool listed() const { return m_listed; }

    /// The folder's path, for the rows Dolphin shows.
    void setRootPath(const QString &root);
    /// Tests set the time.
    void setClock(Clock clock) { m_clock = std::move(clock); }

    /// Re-read after a listing error, keeping the last known holds while it waits.
    Q_INVOKABLE void retry() { requestList(); }
    /// The open group's rows: `kind`'s, held or sent; an empty kind closes it.
    Q_INVOKABLE void showRows(const QString &kind, bool sent = false);
    /// The On Hold page is shown: what was sent before the hold stays on it after the hold is
    /// over, until it is left.
    Q_INVOKABLE void setReviewing(bool reviewing);
    /// Allow and Restore from the page, for one kind or (empty) every kind listed; what they did
    /// lands in results.
    Q_INVOKABLE void allow(const QString &kind = QString());
    Q_INVOKABLE void restore(const QString &kind = QString());
    /// Allow All's question as it stands, for the prompt to keep while it is open: token,
    /// title, text, and kinds, each a map of kind and count.
    Q_INVOKABLE QVariantMap sendAllQuestion() const;
    /// Allow All as `question` (sendAllQuestion()) asked it: its kinds, on its token, never
    /// what was held after it was asked.
    Q_INVOKABLE void allowAsked(const QVariantMap &question);
    Q_INVOKABLE void closeResult(int id);
    /// UploadLocal for a file Don't Upload kept on this computer, from Issues; `path` is its
    /// full path in the folder. A refusal lands in actionError.
    Q_INVOKABLE void uploadLocal(const QString &path);

    /// The same decisions for the notification, on `token` (what it said): `done` gets the
    /// daemon's result, `failed` its refusal; nothing lands in results.
    void decide(bool restore,
                const QStringList &kinds,
                const QString &token,
                std::function<void(const KonedriveGuardResult &)> done,
                std::function<void(const QString &)> failed);

    /// "1.4 GiB", "88 KiB", "0 B": a size with one decimal where it has one.
    static QString sizeText(qulonglong bytes);
    /// "312 deletions", "58 changed files", "1 move", "11,302 new files".
    static QString countText(const QString &kind, qulonglong count);
    /// "312 deletions and 58 changed files": the kinds, joined.
    static QString kindsText(const QList<Kind> &kinds);
    /// The time of the first held change: "14:02", "yesterday at 23:10", or a date.
    QString sinceText(qint64 unixSeconds) const;
    /// What a decision did, in one sentence or two, for `kinds` with `shown` changes listed of
    /// them, and what of them was `sent` before the hold; the type in `type` (positive,
    /// information, warning).
    static QString resultText(bool restore,
                              const QStringList &kinds,
                              qulonglong shown,
                              const KonedriveGuardResult &result,
                              QString *type,
                              const QList<Kind> &sent = {});
    /// The daemon's reasons why changes stay on hold, each said once.
    static QString failureText(const KonedriveGuardResult &result);
    /// What failed stays on hold, and why: "1 is still on hold because it is open in another
    /// program." The daemon says why for each; reasons that differ are on the rows, which the
    /// page shows (`onPage`) and the notification points to.
    static QString stillHeldText(const KonedriveGuardResult &result, bool onPage);
    /// What of `sent` went out before the hold: "20 deleted before the hold are already in
    /// OneDrive's recycle bin."; empty for nothing.
    static QString sentText(const QList<Kind> &sent);

Q_SIGNALS:
    void changed();
    void resultsChanged();
    void actionErrorChanged();

private Q_SLOTS:
    void onRestoreProgress(const QString &account, const QString &token, const QStringList &kinds, qulonglong done, qulonglong total);

private:
    /// A decision under way: its kinds, and how far a restore is.
    struct Call {
        bool restore = false;
        QString token;
        QStringList kinds;
        qulonglong done = 0;
        qulonglong total = 0;
    };

    void requestList();
    void list();
    void take(const QString &token, const KonedriveHeldChangeList &rows);
    void setServiceAvailable(bool available);
    void addResult(const QString &type, const QString &text);
    void decideHere(bool restore, const QString &kind);
    /// The page's decision on `selected`, as listed under `token`.
    void decideOn(bool restore, const QList<Kind> &selected, const QString &token);
    /// What a restore of `kind` (empty: several) brought back, all `shown` of it or not.
    static QString restoredText(const QString &kind, qulonglong n, qulonglong shown);
    QList<Kind> summed(bool sent) const;
    /// What was sent of `kinds` before the hold.
    QList<Kind> sentOf(const QStringList &kinds) const;
    /// The daemon's reasons for the changes of `kinds` that `result` left on hold.
    void takeFailures(const QStringList &kinds, const KonedriveGuardResult &result);
    QVariantMap kindMap(const Kind &kind) const;
    void refreshRows();

    QDBusConnection m_bus;
    QString m_id;
    OrgKonedriveChangeGuardInterface *m_interface;
    QDBusServiceWatcher *m_watcher;
    QTimer *m_listTimer;
    Clock m_clock;
    bool m_serviceAvailable = false;
    bool m_listed = false;
    bool m_listing = false;
    bool m_listAgain = false;
    bool m_reviewing = false;
    QString m_listingError;
    QString m_token;
    KonedriveHeldChangeList m_held;
    KonedriveHeldChangeList m_sent;
    QString m_root;
    HeldRowsModel *m_rows;
    QString m_rowsKind;
    bool m_rowsSent = false;
    /// Why the last decision of each kind left changes on hold: by kind, then path.
    QHash<QString, QHash<QString, QString>> m_failures;
    /// The decisions under way, by number.
    QHash<quint64, Call> m_calls;
    quint64 m_lastCall = 0;
    /// Restores the window did not ask (konedrivectl), as RestoreProgress says them, by kinds.
    QHash<QString, Call> m_otherRestores;
    QVariantList m_results;
    int m_lastResult = 0;
    QString m_actionError;
};
