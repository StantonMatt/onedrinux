// OneDrive's stored facets, without ever opening the file. The daemon serves
// the media attribute only while content is off disk; the visual state (which
// can say "hydrating" for on-disk content) must not be consulted here.
#include <KFileMetaData/ExtractorPlugin>

#include "mediafacts.h"

using namespace KFileMetaData;

class KonedriveMediaExtractor : public ExtractorPlugin
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID kfilemetadata_extractor_iid FILE "konedrive_mediaextractor.json")
    Q_INTERFACES(KFileMetaData::ExtractorPlugin)

public:
    KonedriveMediaExtractor()
        : ExtractorPlugin(nullptr)
    {
    }

    QStringList mimetypes() const override
    {
        // Union of KF6 6.24's exiv2 and ffmpeg lists, including BMFF types.
        return {QStringLiteral("image/avif"), QStringLiteral("image/bmp"), QStringLiteral("image/gif"),
                QStringLiteral("image/heif"), QStringLiteral("image/jp2"), QStringLiteral("image/jpeg"),
                QStringLiteral("image/jxl"), QStringLiteral("image/pgf"), QStringLiteral("image/png"),
                QStringLiteral("image/tiff"), QStringLiteral("image/webp"), QStringLiteral("image/x-canon-cr2"),
                QStringLiteral("image/x-canon-cr3"), QStringLiteral("image/x-canon-crw"), QStringLiteral("image/x-exv"),
                QStringLiteral("image/x-fuji-raf"), QStringLiteral("image/x-minolta-mrw"), QStringLiteral("image/x-nikon-nef"),
                QStringLiteral("image/x-olympus-orf"), QStringLiteral("image/x-panasonic-rw2"), QStringLiteral("image/x-pentax-pef"),
                QStringLiteral("image/x-photoshop"), QStringLiteral("image/x-samsung-srw"), QStringLiteral("image/x-tga"),
                QStringLiteral("video/mp2t"), QStringLiteral("video/mp4"), QStringLiteral("video/mpeg"),
                QStringLiteral("video/ogg"), QStringLiteral("video/quicktime"), QStringLiteral("video/vnd.avi"),
                QStringLiteral("video/webm"), QStringLiteral("video/x-flv"), QStringLiteral("video/x-matroska"),
                QStringLiteral("video/x-ms-asf"), QStringLiteral("video/x-ms-wmv"), QStringLiteral("video/x-msvideo")};
    }

    void extract(ExtractionResult *result) override
    {
        if (!(result->inputFlags() & ExtractionResult::ExtractMetaData)) {
            return;
        }
        konedrive::addMediaFacts(konedrive::readMediaAttribute(result->inputUrl()), result);
    }
};

#include "mediaextractor.moc"
