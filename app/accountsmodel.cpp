#include "accountsmodel.h"

#include "accountcontroller.h"
#include "accountinterface.h"
#include "daemoncontroller.h"
#include "synccontroller.h"

#include <KLocalizedString>

#include <QClipboard>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusVariant>
#include <QGuiApplication>
#include <QRegularExpression>

namespace
{
const QLatin1String ReadOnly("read-only");
}

AccountItem::AccountItem(const QDBusConnection &bus, const QString &path, const AccountStatus::Clock &clock, QObject *parent)
    : QObject(parent)
    , m_path(path)
    , m_account(new AccountController(bus, path, this))
    , m_sync(new SyncController(bus, path, this))
    , m_held(new HeldChanges(bus, path, this))
    , m_status(new AccountStatus(m_account, m_sync, clock, this))
{
    // Its rows show in Dolphin from the folder.
    m_held->setRootPath(m_sync->rootPath());
    connect(m_sync, &SyncController::syncChanged, m_held, [this] {
        m_held->setRootPath(m_sync->rootPath());
    });
    m_status->setHeld(m_held);
}

QString AccountItem::id() const
{
    return m_account->id();
}

AccountsModel::AccountsModel(DaemonController *daemon, AccountStatus::Clock clock, QObject *parent)
    : QAbstractListModel(parent)
    , m_daemon(daemon)
    , m_clock(std::move(clock))
{
    connect(m_daemon, &DaemonController::accountsChanged, this, [this] {
        follow(m_daemon->accounts());
    });
    connect(m_daemon, &DaemonController::signInFinished, this, &AccountsModel::handleSignInFinished);
    connect(m_daemon, &DaemonController::serviceAvailableChanged, this, &AccountsModel::handleServiceChanged);
    follow(m_daemon->accounts());
}

int AccountsModel::rowCount(const QModelIndex &parent) const
{
    return parent.isValid() ? 0 : int(m_items.size());
}

QVariant AccountsModel::data(const QModelIndex &index, int role) const
{
    const AccountItem *item = index.isValid() ? at(index.row()) : nullptr;
    if (!item) {
        return {};
    }
    switch (role) {
    case Qt::DisplayRole:
    case LabelRole:
        return item->account()->label();
    case PathRole:
        return item->path();
    case IdRole:
        return item->id();
    case EmailRole:
        return item->account()->email();
    case AccountStateRole:
        return item->account()->state();
    case StatusStateRole:
        return item->status()->state();
    case IconNameRole:
        return item->status()->iconName();
    case StatusTextRole:
        return item->status()->text();
    case RootPathRole:
        return item->sync()->rootPath();
    case ItemRole:
        return QVariant::fromValue(const_cast<AccountItem *>(item));
    default:
        return {};
    }
}

QHash<int, QByteArray> AccountsModel::roleNames() const
{
    return {
        {PathRole, "path"},
        {IdRole, "accountId"},
        {LabelRole, "label"},
        {EmailRole, "email"},
        {AccountStateRole, "accountState"},
        {StatusStateRole, "statusState"},
        {IconNameRole, "iconName"},
        {StatusTextRole, "statusText"},
        {RootPathRole, "rootPath"},
        {ItemRole, "item"},
    };
}

int AccountsModel::heldCount() const
{
    int count = 0;
    for (const AccountItem *item : m_items) {
        count += item->held()->count();
    }
    return count;
}

bool AccountsModel::anySignedIn() const
{
    for (const AccountItem *item : m_items) {
        if (item->account()->state() != QLatin1String("signed-out")) {
            return true;
        }
    }
    return false;
}

AccountItem *AccountsModel::find(const QString &path) const
{
    return at(indexOf(path));
}

int AccountsModel::indexOf(const QString &path) const
{
    for (int row = 0; row < m_items.size(); ++row) {
        if (m_items.at(row)->path() == path) {
            return row;
        }
    }
    return -1;
}

void AccountsModel::onEachAccount(std::function<void(AccountItem *)> setUp)
{
    for (AccountItem *item : std::as_const(m_items)) {
        setUp(item);
    }
    m_setUps.push_back(std::move(setUp));
}

void AccountsModel::follow(const QStringList &paths)
{
    for (int row = int(m_items.size()) - 1; row >= 0; --row) {
        if (!paths.contains(m_items.at(row)->path())) {
            removeAt(row);
        }
    }
    for (int i = 0; i < paths.size(); ++i) {
        const int row = indexOf(paths.at(i));
        if (row == i) {
            continue;
        }
        if (row < 0) {
            insert(i, paths.at(i));
            continue;
        }
        // The daemon never reorders its list; followed all the same.
        beginMoveRows(QModelIndex(), row, row, QModelIndex(), i);
        m_items.move(row, i);
        endMoveRows();
    }
    if (!m_awaitedPath.isEmpty() && indexOf(m_awaitedPath) >= 0) {
        showAdded();
    }
}

AccountItem *AccountsModel::insert(int row, const QString &path)
{
    auto *item = new AccountItem(m_daemon->bus(), path, m_clock, this);
    const auto changed = [this, item] {
        rowChanged(item);
    };
    connect(item->account(), &AccountController::accountChanged, this, changed);
    connect(item->account(), &AccountController::serviceAvailableChanged, this, changed);
    connect(item->sync(), &SyncController::syncChanged, this, changed);
    connect(item->sync(), &SyncController::serviceAvailableChanged, this, changed);
    connect(item->status(), &AccountStatus::changed, this, changed);
    connect(item->held(), &HeldChanges::changed, this, &AccountsModel::heldCountChanged);
    connect(item->account(), &AccountController::openUrlRequested, this, &AccountsModel::openUrlRequested);

    beginInsertRows(QModelIndex(), row, row);
    m_items.insert(row, item);
    endInsertRows();
    Q_EMIT countChanged();
    Q_EMIT summaryChanged();
    for (const auto &setUp : m_setUps) {
        setUp(item);
    }
    return item;
}

void AccountsModel::removeAt(int row)
{
    AccountItem *item = m_items.at(row);
    beginRemoveRows(QModelIndex(), row, row);
    m_items.removeAt(row);
    endRemoveRows();
    Q_EMIT countChanged();
    Q_EMIT heldCountChanged();
    Q_EMIT summaryChanged();
    Q_EMIT accountRemoved(item);
    // Later: QML may still hold it for the rest of this event.
    item->deleteLater();
}

void AccountsModel::rowChanged(AccountItem *item)
{
    const int row = indexOf(item->path());
    if (row < 0) {
        return;
    }
    Q_EMIT dataChanged(index(row), index(row));
    Q_EMIT summaryChanged();
}

QString AccountsModel::labelProblem(const QString &label, const QString &exceptPath) const
{
    const QString name = label.trimmed();
    if (name.isEmpty()) {
        return i18n("Enter a name for the account.");
    }
    // The daemon counts characters, not UTF-16 units.
    if (name.toUcs4().size() > 40) {
        return i18n("A name can be at most 40 characters long.");
    }
    if (name.contains(QLatin1Char('/'))) {
        return i18n("A name cannot contain “/”.");
    }
    for (const QChar c : name) {
        if (c.category() == QChar::Other_Control) {
            return i18n("A name cannot contain control characters.");
        }
    }
    // What an account id looks like, so `--account` could not tell them apart.
    static const QRegularExpression idLike(QStringLiteral("^[0-9a-fA-F]{12}$"));
    if (idLike.match(name).hasMatch()) {
        return i18n("A name cannot be 12 hexadecimal digits: that is what an account ID looks like.");
    }
    for (const AccountItem *item : m_items) {
        if (item->path() != exceptPath && item->account()->label().compare(name, Qt::CaseInsensitive) == 0) {
            return i18n("Another account is already called %1.", item->account()->label());
        }
    }
    return QString();
}

void AccountsModel::addAccount(const QString &clientId)
{
    if (m_adding) {
        return;
    }
    m_adding = true;
    m_addError.clear();
    m_addMode.clear();
    Q_EMIT addingChanged();

    // An answer that comes once this adding has ended (the daemon went away
    // in the middle) is not this adding's, nor a later one's.
    const quint64 attempt = ++m_attempt;
    const QString id = clientId.trimmed();
    if (!id.isEmpty() && id != m_daemon->clientId()) {
        m_daemon->setClientId(
            id,
            [this, attempt] {
                startSignIn(attempt);
            },
            [this, attempt](const QString &error) {
                addFailed(attempt, error);
            });
    } else {
        startSignIn(attempt);
    }
}

void AccountsModel::startSignIn(quint64 attempt)
{
    if (!m_adding || attempt != m_attempt) {
        return;
    }
    m_daemon->add(
        m_addMode,
        [this, attempt](uint number, const QString &url) {
            if (!m_adding || attempt != m_attempt) {
                return;
            }
            m_signIn = number;
            // The daemon may have said how the sign-in ended before this
            // answer was handled.
            const QStringList early = m_early.take(number);
            m_early.clear();
            if (!early.isEmpty()) {
                signInEnded(early.at(0), early.at(1), early.at(2));
            } else if (m_cancelRequested) {
                cancelSignIn();
            } else {
                m_signInUrl = url;
                Q_EMIT addingChanged();
                Q_EMIT openUrlRequested(url);
            }
        },
        [this, attempt](const QString &error) {
            addFailed(attempt, error);
        });
}

void AccountsModel::addFailed(quint64 attempt, const QString &error)
{
    if (!m_adding || attempt != m_attempt) {
        return;
    }
    if (m_cancelRequested) {
        // Cancel was pressed first: there is nothing to cancel, and nothing to say.
        finishAdding(QString());
    } else {
        finishAdding(error.isEmpty() ? i18n("The account could not be added.") : error);
    }
}

void AccountsModel::addReadOnlyInstead()
{
    if (!m_adding || !m_signIn || m_cancelRequested || !m_awaitedPath.isEmpty() || m_addMode == ReadOnly) {
        return;
    }
    // Cancel first; if the browser finished meanwhile, make its account read-only instead.
    const uint number = *m_signIn;
    const quint64 attempt = ++m_attempt;
    m_addMode = ReadOnly;
    m_replacing = number;
    m_signInUrl.clear();
    Q_EMIT addingChanged();
    m_daemon->cancelSignIn(
        number,
        [this, attempt](const QString &error) {
            addFailed(attempt, error);
        },
        [this, attempt, number](bool cancelled) {
            if (!m_adding || attempt != m_attempt) {
                return;
            }
            m_replacing.reset();
            if (!cancelled) {
                // It ended by itself meanwhile; its SignInFinished says how.
                return;
            }
            if (m_signIn == number) {
                m_signIn.reset();
            }
            if (m_cancelRequested) {
                finishAdding(QString());
                return;
            }
            startSignIn(attempt);
        });
}

void AccountsModel::handleSignInFinished(uint number, const QString &outcome, const QString &message, const QString &account)
{
    if (!m_adding) {
        return;
    }
    if (!m_signIn) {
        // SignIn has not answered: which sign-in is this window's is not known yet.
        m_early.insert(number, {outcome, message, account});
        return;
    }
    // The two-way sign-in given up for a read-only one: its "cancelled" ends nothing.
    if (m_replacing == number && outcome == QLatin1String("cancelled")) {
        return;
    }
    // Another number is another client's sign-in.
    if (number == *m_signIn && m_awaitedPath.isEmpty()) {
        signInEnded(outcome, message, account);
    }
}

void AccountsModel::handleServiceChanged()
{
    if (!m_adding || m_daemon->serviceAvailable()) {
        return;
    }
    // A daemon that stops sends no SignInFinished, and its sign-in went with
    // it. A sign-in that was being cancelled ends as it was asked to.
    finishAdding(m_cancelRequested ? QString() : i18n("The KOneDrive service stopped before the account was added. Sign in again."));
}

void AccountsModel::signInEnded(const QString &outcome, const QString &message, const QString &account)
{
    if (outcome == QLatin1String("signed-in")) {
        // Accounts.List changes before the signal is sent; waited for all the same.
        m_awaitedPath = account;
        if (indexOf(account) >= 0) {
            showAdded();
        }
    } else if (outcome == QLatin1String("cancelled")) {
        // By cancelAdd(), or by a newer sign-in started elsewhere.
        finishAdding(QString());
    } else if (outcome == QLatin1String("already-added")) {
        finishAdding(i18n("This account is already added as %1.", message));
    } else {
        finishAdding(message.isEmpty() ? i18n("The account could not be added.") : message);
    }
}

void AccountsModel::showAdded()
{
    if (m_finishingAdd) {
        return;
    }
    const QString path = m_awaitedPath;
    if (m_addMode != ReadOnly) {
        finishAdding(QString());
        Q_EMIT accountAdded(path);
        return;
    }
    // Cancellation can lose to browser completion. Enforce the choice even then, before
    // onboarding can pick a folder. Force keeps any unsent work here and stops uploads now.
    m_finishingAdd = true;
    const quint64 attempt = m_attempt;
    OrgKonedriveAccountInterface account(DaemonController::ServiceName, path, m_daemon->bus());
    auto *watcher = new QDBusPendingCallWatcher(account.SetMode(ReadOnly, true), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, attempt, path](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (!m_adding || attempt != m_attempt) {
            return;
        }
        const QDBusPendingReply<QString> reply = *w;
        if (reply.isError()) {
            finishAdding(i18n("The account could not be made read-only: %1", reply.error().message()));
            return;
        }
        auto get = QDBusMessage::createMethodCall(DaemonController::ServiceName, path,
                                                 QStringLiteral("org.freedesktop.DBus.Properties"), QStringLiteral("Get"));
        get << AccountController::InterfaceName << QStringLiteral("Mode");
        auto *check = new QDBusPendingCallWatcher(m_daemon->bus().asyncCall(get), this);
        connect(check, &QDBusPendingCallWatcher::finished, this, [this, attempt, path](QDBusPendingCallWatcher *w) {
            w->deleteLater();
            if (!m_adding || attempt != m_attempt) {
                return;
            }
            const QDBusPendingReply<QDBusVariant> reply = *w;
            if (reply.isError() || reply.value().variant().toString() != ReadOnly) {
                finishAdding(i18n("The account's read-only mode could not be verified. Check OneDrive Settings before choosing a folder."));
                return;
            }
            finishAdding(QString());
            Q_EMIT accountAdded(path);
        });
    });
}

void AccountsModel::cancelAdd()
{
    if (!m_adding) {
        return;
    }
    m_cancelRequested = true;
    if (!m_signIn) {
        // SignIn (or SetClientId before it) is still in flight: the sign-in
        // is cancelled as soon as it has a number.
        return;
    }
    if (!m_awaitedPath.isEmpty()) {
        // Signed in already; only its row has not come.
        finishAdding(QString());
    } else {
        cancelSignIn();
    }
}

void AccountsModel::cancelSignIn()
{
    // The daemon says how the sign-in ended with SignInFinished, whatever the
    // call answers: "cancelled" when it cancelled, and otherwise the outcome
    // the sign-in had by itself, "signed-in" when the account was being made
    // already. A call that gets no answer ends the wait: nothing more will come.
    const uint number = *m_signIn;
    m_daemon->cancelSignIn(number, [this, number](const QString &) {
        if (m_adding && m_signIn == number && m_awaitedPath.isEmpty()) {
            finishAdding(QString());
        }
    });
}

void AccountsModel::finishAdding(const QString &error)
{
    m_finishingAdd = false;
    m_signIn.reset();
    m_replacing.reset();
    m_addMode.clear();
    m_cancelRequested = false;
    m_awaitedPath.clear();
    m_early.clear();
    m_adding = false;
    m_addError = error;
    m_signInUrl.clear();
    Q_EMIT addingChanged();
}

void AccountsModel::clearAddError()
{
    if (m_adding || m_addError.isEmpty()) {
        return;
    }
    m_addError.clear();
    Q_EMIT addingChanged();
}

void AccountsModel::copySignInUrl() const
{
    if (auto *clipboard = QGuiApplication::clipboard(); clipboard && !m_signInUrl.isEmpty()) {
        clipboard->setText(m_signInUrl);
    }
}

void AccountsModel::removeAccount(const QString &path)
{
    m_daemon->remove(path);
}

void AccountsModel::retry()
{
    m_daemon->retry();
    for (AccountItem *item : std::as_const(m_items)) {
        item->account()->retry();
        item->sync()->retry();
    }
}
