// A child with Baloo's wire protocol, including failures and a hung read.
// It never reads the selected file. Tests watch that file for IN_OPEN.
#include <KFileMetaData/Properties>
#include <QCoreApplication>
#include <QDataStream>
#include <QFile>
#include <QFileInfo>
#include <QTimer>
#include <csignal>
#include <unistd.h>
using namespace KFileMetaData;
int main(int argc, char **argv)
{
    QCoreApplication app(argc, argv);
    const QString path = app.arguments().value(1);
    const QString mode = QFileInfo(path).baseName();
    QFile log(qEnvironmentVariable("KONEDRIVE_TEST_HELPER_LOG"));
    if (!log.open(QIODevice::WriteOnly | QIODevice::Append)) return 2;
    log.write(QByteArray::number(::getpid()) + ' ' + QFile::encodeName(path) + '\n');
    log.close();
    if (mode.startsWith(QLatin1String("timeout"))) return app.exec();
    if (mode == QLatin1String("crash")) { ::raise(SIGKILL); return 1; }
    if (mode == QLatin1String("failure")) return 1;
    QFile out;
    out.open(stdout, QIODevice::WriteOnly);
    if (mode == QLatin1String("garbage")) { out.write("invalid reply"); return 0; }
    if (mode == QLatin1String("oversized")) { out.write(QByteArray(2 * 1024 * 1024, 'x')); return 0; }
    const auto reply = [&out, mode] {
        PropertyMultiMap properties;
        if (mode != QLatin1String("empty")) {
            properties = {{Property::Width, 80}, {Property::Height, 50},
                {Property::Model, QStringLiteral("Current EXIF")},
                {Property::Manufacturer, QStringLiteral("<b>literal</b>")},
                {Property::Artist, QStringLiteral("Local author")},
                {Property::Rating, 5}, {Property::Comment, QStringLiteral("excluded")}};
        }
        // The installed Baloo helper uses quint16 property IDs via its
        // extractorutil_p.h adapter, independently of the enum's C++ size.
        QMultiMap<quint16, QVariant> wire;
        for (auto it = properties.cend(); it != properties.cbegin();) {
            --it;
            wire.insert(static_cast<quint16>(it.key()), it.value());
        }
        if (mode == QLatin1String("unknown-property")) {
            wire.clear();
            wire.insert(65535, QStringLiteral("invalid property"));
        }
        QDataStream stream(&out);
        stream << wire;
        out.flush();
        QCoreApplication::quit();
    };
    if (mode == QLatin1String("delayed")) {
        QTimer::singleShot(200, &app, reply);
        return app.exec();
    }
    reply();
    return 0;
}
