#include "detailstab.h"
#include "detailsrows.h"
#include "mediafacts.h"

#include <KFileMetaData/SimpleExtractionResult>
#include <KFileMetaData/PropertyInfo>
#include <KLibexec>
#include <KLocalizedString>
#include <KPageWidget>
#include <KPropertiesDialog>
#include <QCoreApplication>
#include <QDataStream>
#include <QMimeDatabase>
#include <QProcess>
#include <QStandardPaths>

namespace konedrive {
namespace {
QString title()
{
    return i18ndc("baloowidgets5", "Tab page with file meta data", "&Details");
}
QString withoutAccelerator(QString text)
{
    return text.remove(QLatin1Char('&'));
}
} // namespace

QString DetailsTab::probeExecutable()
{
    // Beside the uninstalled plugin for local builds; libexec for installs.
    return QStandardPaths::findExecutable(QStringLiteral("konedrive_details_probe"),
        {KLibexec::path(QStringLiteral(".")), QStringLiteral(KONEDRIVE_DETAILS_LIBEXECDIR)});
}

QString DetailsTab::extractorExecutable()
{
    const QString name = QStringLiteral("baloo_filemetadata_temp_extractor");
    const QString inPath = QStandardPaths::findExecutable(name);
    if (!inPath.isEmpty()) return inPath;
    auto paths = KLibexec::kdeFrameworksPaths(QStringLiteral(""));
    paths << QStringLiteral("/usr/libexec") << QStringLiteral("/usr/libexec/kf6")
          << QStringLiteral("/usr/lib/qt6/libexec") << QStringLiteral(KONEDRIVE_DETAILS_LIBEXECDIR);
    return QStandardPaths::findExecutable(name, paths);
}

DetailsTab::DetailsTab(KPropertiesDialog *dialog, const KFileItemList &items,
                       const QString &probe, const QString &extractor, int timeoutMs)
    : QObject(dialog), m_dialog(dialog), m_extractor(extractor), m_timeoutMs(timeoutMs)
{
    m_timeout.setSingleShot(true);
    connect(&m_timeout, &QTimer::timeout, this, &DetailsTab::cancel);
    connect(dialog, &QDialog::finished, this, &DetailsTab::cancel);
    if (auto pages = dialog->findChild<KPageWidget *>()) {
        // Baloo may add its page only after its own extraction finishes.
        connect(pages->model(), &QAbstractItemModel::rowsInserted, this, [this] {
            QTimer::singleShot(0, this, &DetailsTab::checkCoexistence);
        });
        connect(pages->model(), &QAbstractItemModel::dataChanged, this, [this] {
            QTimer::singleShot(0, this, &DetailsTab::checkCoexistence);
        });
    }
    // Defer even the fast rejection until callers can connect settled().
    QTimer::singleShot(0, this, [this, items, probe] {
        if (m_done) return;
        if (items.size() != 1 || !items.first().url().isLocalFile() || items.first().isDir() || items.first().isLink() || otherDetails()) {
            finish();
            return;
        }
        m_path = items.first().url().toLocalFile();
        // MatchExtension must be explicit: QMimeDatabase's default can open
        // content. Do not ask KFileItem::mimetype(), which may sniff it too.
        const QString mime = QMimeDatabase().mimeTypeForFile(m_path, QMimeDatabase::MatchExtension).name();
        if (!mime.startsWith(QLatin1String("image/")) && !mime.startsWith(QLatin1String("video/"))) {
            finish();
            return;
        }
        start(probe, true);
    });
}

DetailsTab::~DetailsTab()
{
    cancel();
}

void DetailsTab::start(const QString &executable, bool probe)
{
    if (executable.isEmpty()) {
        finish();
        return;
    }
    m_output.clear();
    // Owned by the application while running, so dialog destruction never
    // waits in QProcess's destructor for a child blocked in the filesystem.
    auto process = new QProcess(QCoreApplication::instance());
    m_process = process;
    process->setStandardErrorFile(QProcess::nullDevice());
    connect(process, &QProcess::finished, process, &QObject::deleteLater);
    connect(process, &QProcess::errorOccurred, process, [process](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart) process->deleteLater();
    });
    connect(process, &QProcess::readyReadStandardOutput, this, [this, process] {
        m_output += process->readAllStandardOutput();
        if (m_output.size() > 1024 * 1024) cancel();
    });
    connect(process, &QProcess::errorOccurred, this, [this](QProcess::ProcessError) { cancel(); });
    connect(process, &QProcess::finished, this, [this, process, probe](int code, QProcess::ExitStatus status) {
        if (m_done) return;
        m_timeout.stop();
        m_output += process->readAllStandardOutput();
        m_process = nullptr;
        if (code != 0 || status != QProcess::NormalExit || m_output.size() > 1024 * 1024 || otherDetails()) {
            finish();
            return;
        }
        KFileMetaData::PropertyMultiMap facts;
        QDataStream stream(&m_output, QIODevice::ReadOnly);
        if (probe) {
            bool eligible = false, hydrated = false;
            QByteArray payload;
            stream >> eligible >> payload >> hydrated;
            if (stream.status() != QDataStream::Ok || !stream.atEnd() || !eligible) {
                finish();
                return;
            }
            KFileMetaData::SimpleExtractionResult result(m_path, QString(), KFileMetaData::ExtractionResult::ExtractMetaData);
            addMediaFacts(payload, &result);
            // A valid contract with no fields is still authoritative.
            if (result.types().isEmpty() && hydrated) {
                start(m_extractor, false);
                return;
            }
            facts = result.properties();
        } else {
            // Baloo Widgets' extractorutil_p.h serializes property IDs as
            // quint16, not as QDataStream's default enum representation.
            QMultiMap<quint16, QVariant> wire;
            stream >> wire;
            if (stream.status() != QDataStream::Ok || !stream.atEnd()) {
                finish();
                return;
            }
            // Reverse insertion preserves the multimap's value order.
            for (auto it = wire.cend(); it != wire.cbegin();) {
                --it;
                const auto property = static_cast<KFileMetaData::Property::Property>(it.key());
                if (property == KFileMetaData::Property::Empty
                    || KFileMetaData::PropertyInfo(property).property() != property || !it.value().isValid()) {
                    finish();
                    return;
                }
                facts.insert(property, it.value());
            }
        }
        const auto data = visibleDetails(facts);
        if (!data.isEmpty()) {
            const auto current = m_dialog->currentPage();
            m_page = m_dialog->addPage(detailsRows(data), title());
            if (current) m_dialog->setCurrentPage(current);
        }
        finish();
    });
    m_timeout.start(m_timeoutMs);
    process->start(executable, {m_path});
}

void DetailsTab::finish()
{
    m_timeout.stop();
    if (!m_done) {
        m_done = true;
        Q_EMIT settled();
    }
}

void DetailsTab::cancel()
{
    if (m_process) {
        m_process->disconnect(this);
        m_process->kill();
        m_process = nullptr;
    }
    finish();
}

bool DetailsTab::otherDetails() const
{
    if (m_suppressed) return true;
    const auto pages = m_dialog->findChild<KPageWidget *>();
    const auto model = pages ? qobject_cast<KPageWidgetModel *>(pages->model()) : nullptr;
    if (!model) return false;
    for (int row = 0; row < model->rowCount(); ++row) {
        const auto page = model->item(model->index(row, 0));
        if (page && page != m_page && withoutAccelerator(page->name()) == withoutAccelerator(title())) return true;
    }
    return false;
}

void DetailsTab::checkCoexistence()
{
    if (otherDetails()) {
        m_suppressed = true;
        cancel();
        if (m_page) m_dialog->removePage(m_page);
    }
}
} // namespace konedrive
