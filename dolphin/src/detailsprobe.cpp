// Bounded out-of-process path/xattr probe: even a wedged FUSE server must
// never block the Properties dialog. Opens no file content. The tab parses
// and maps the returned media JSON in Dolphin's process.
#include "filestate.h"
#include "mediafacts.h"
#include <QCoreApplication>
#include <QDataStream>
#include <QFile>
#include <cerrno>
#include <sys/xattr.h>

int main(int argc, char **argv)
{
    QCoreApplication app(argc, argv);
    const auto args = app.arguments();
    if (args.size() != 2) {
        return 1;
    }
    const QString path = args.at(1);
    const auto state = konedrive::readFileState(path);
    const bool eligible = state != konedrive::FileState::NotAFile
        && konedrive::rootOf(konedrive::parentDirectory(path)).has_value();
    // Unmanaged also covers xattr errors. Admit only ENODATA: a regular
    // file created locally has no cloud state until its first upload.
    const auto native = QFile::encodeName(path);
    const bool localOnly = eligible && state == konedrive::FileState::Unmanaged
        && ::lgetxattr(native.constData(), konedrive::StateAttribute, nullptr, 0) == -1
        && errno == ENODATA;
    QFile out;
    if (!out.open(stdout, QIODevice::WriteOnly)) {
        return 1;
    }
    QDataStream stream(&out);
    stream << eligible << (eligible ? konedrive::readMediaAttribute(path) : QByteArray())
           << (eligible && (state == konedrive::FileState::Hydrated || localOnly));
    return stream.status() == QDataStream::Ok ? 0 : 1;
}
