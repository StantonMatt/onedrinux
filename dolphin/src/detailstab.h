// Asynchronous Properties fallback. Both mount syscalls and content extraction
// run in killable children; the dialog thread handles only their replies.
#pragma once
#include <KFileItem>
#include <QObject>
#include <QPointer>
#include <QTimer>
class KPropertiesDialog;
class KPageWidgetItem;
class QProcess;

namespace konedrive {
class DetailsTab : public QObject
{
    Q_OBJECT
public:
    explicit DetailsTab(KPropertiesDialog *dialog, const KFileItemList &items,
                        const QString &probe = probeExecutable(), const QString &extractor = extractorExecutable(),
                        int timeoutMs = 5000);
    ~DetailsTab() override;
    static QString probeExecutable();
    static QString extractorExecutable();
Q_SIGNALS:
    void settled();
private:
    void start(const QString &executable, bool probe);
    void finish();
    void cancel();
    bool otherDetails() const;
    void checkCoexistence();
    KPropertiesDialog *const m_dialog;
    QPointer<KPageWidgetItem> m_page;
    QPointer<QProcess> m_process;
    QTimer m_timeout;
    QByteArray m_output;
    QString m_path;
    const QString m_extractor;
    const int m_timeoutMs;
    bool m_done = false;
    bool m_suppressed = false;
};
} // namespace konedrive
