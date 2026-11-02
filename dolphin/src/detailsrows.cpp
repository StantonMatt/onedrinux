/*
    SPDX-FileCopyrightText: 2008 Sebastian Trueg <trueg@kde.org>
    SPDX-FileCopyrightText: 2009-2010 Peter Penz <peter.penz@gmx.at>
    SPDX-FileCopyrightText: 2012-2013 Vishesh Handa <me@vhanda.in>
    SPDX-FileCopyrightText: 2012-2014 Vishesh Handa <vhanda@kde.org>
    SPDX-FileCopyrightText: 2021 Kai Uwe Broulik <kde@broulik.de>
    SPDX-License-Identifier: GPL-3.0-or-later

    Adapted from Baloo Widgets' FileMetaDataWidget, FileMetaDataProvider,
    MetadataFilter and WidgetFactory (LGPL-2.0-or-later / LGPL-2.1-or-later).
    This adaptation uses the GPL option permitted by section 3 of the LGPL.
*/

// Row order/layout/default visibility follow Baloo Widgets 25.12's
// FileMetaDataWidget, FileMetaDataProvider and MetadataFilter. All labels
// and value formats come from KDE. No editable user metadata is offered.
#include "detailsrows.h"
#include <KConfig>
#include <KConfigGroup>
#include <KFileMetaData/PropertyInfo>
#include <KFormat>
#include <KLocalizedString>
#include <QDateTime>
#include <QGridLayout>
#include <QLabel>
#include <QScrollArea>
#include <QVBoxLayout>
using namespace KFileMetaData;
namespace konedrive {
namespace {
class ValueWidget : public QLabel {
public:
    using QLabel::QLabel;
    QSize sizeHint() const override { return QFontMetrics(font()).size(Qt::TextSingleLine, text()); }
};
} // namespace
QVariantMap visibleDetails(const PropertyMultiMap &properties)
{
    QVariantMap data;
    for (auto it = properties.constBegin(); it != properties.constEnd();) {
        const auto key = it.key();
        const PropertyInfo info(key);
        const QVariant first = it.value();
        ++it;
        if (it == properties.constEnd() || it.key() != key) {
            data.insert(info.name(), first);
            continue;
        }
        // Visit each equal-key range once, in Baloo's forward iterator order.
        QVariantList values{first};
        do {
            values.append(it.value());
            ++it;
        } while (it != properties.constEnd() && it.key() == key);
        data.insert(info.name(), values);
    }
    if (data.contains(QStringLiteral("width")) && data.contains(QStringLiteral("height"))) {
        data.insert(QStringLiteral("dimensions"), QSize(data.value(QStringLiteral("width")).toInt(), data.value(QStringLiteral("height")).toInt()));
    }
    // Baloo derives a single location row from local EXIF coordinates;
    // the cloud contract never contains these fields.
    bool latitudeOk = false, longitudeOk = false;
    const auto latitude = data.value(QStringLiteral("photoGpsLatitude")).toFloat(&latitudeOk);
    const auto longitude = data.value(QStringLiteral("photoGpsLongitude")).toFloat(&longitudeOk);
    if (latitudeOk && longitudeOk) {
        data.insert(QStringLiteral("gpsLocation"), QVariant::fromValue(QPair<float, float>(latitude, longitude)));
    }
        static const char *const hiddenByDefault[] = {
            "width",
            "height", // replaced by dimensions
            "comment",
            "contentSize",
            "depends",
            "lastModified",
            "created",
            "contentCreated",
            "mimeType",
            "url",
            "channels",
            "fileName",
            "fileSize",
            "kfileitem#owner",
            "kfileitem#group",
            "kfileitem#permissions",
            "replayGainAlbumPeak",
            "replayGainAlbumGain",
            "replayGainTrackPeak",
            "replayGainTrackGain",
            "embeddedRating",
            "lyrics",
            "photoWhiteBalance",
            "photoMeteringMode",
            "photoSharpness",
            "photoSaturation",
            "photoPixelXDimension",
            "photoPixelYDimension",
            "photoGpsLongitude",
            "photoGpsLatitude",
        };
    KConfig config(QStringLiteral("baloofileinformationrc"), KConfig::NoGlobals);
    const KConfigGroup show = config.group(QStringLiteral("Show"));
    const QStringList excluded{QStringLiteral("mimeType"), QStringLiteral("fileSize"), QStringLiteral("fileName"),
        QStringLiteral("url"), QStringLiteral("lastModified"), QStringLiteral("created"), QStringLiteral("contentCreated"),
        QStringLiteral("tags"), QStringLiteral("rating"), QStringLiteral("userComment"), QStringLiteral("comment"), QStringLiteral("embeddedRating")};
    for (auto it = data.begin(); it != data.end();) {
        bool byDefault = true;
        for (const char *key : hiddenByDefault) {
            if (it.key() == QLatin1String(key)) byDefault = false;
        }
        if (it.key().isEmpty() || excluded.contains(it.key()) || !show.readEntry(it.key(), byDefault)) {
            it = data.erase(it);
        } else {
            ++it;
        }
    }
    return data;
}
QString group(const QString &k)
{
    static const QHash<QString, QString> g = {
        // Image Data
        {QStringLiteral("width"), QStringLiteral("2ImageA")},
        {QStringLiteral("height"), QStringLiteral("2ImageB")},
        {QStringLiteral("dimensions"), QStringLiteral("2ImageCA")},
        {QStringLiteral("photoFNumber"), QStringLiteral("2ImageC")},
        {QStringLiteral("photoExposureTime"), QStringLiteral("2ImageD")},
        {QStringLiteral("photoExposureBiasValue"), QStringLiteral("2ImageE")},
        {QStringLiteral("photoISOSpeedRatings"), QStringLiteral("2ImageF")},
        {QStringLiteral("photoFocalLength"), QStringLiteral("2ImageG")},
        {QStringLiteral("photoFocalLengthIn35mmFilm"), QStringLiteral("2ImageH")},
        {QStringLiteral("photoFlash"), QStringLiteral("2ImageI")},
        {QStringLiteral("imageOrientation"), QStringLiteral("2ImageJ")},
        {QStringLiteral("photoGpsLocation"), QStringLiteral("2ImageK")},
        {QStringLiteral("photoGpsLatitude"), QStringLiteral("2ImageL")},
        {QStringLiteral("photoGpsLongitude"), QStringLiteral("2ImageM")},
        {QStringLiteral("photoGpsAltitude"), QStringLiteral("2ImageN")},
        {QStringLiteral("manufacturer"), QStringLiteral("2ImageO")},
        {QStringLiteral("model"), QStringLiteral("2ImageP")},

        // Media Data
        {QStringLiteral("title"), QStringLiteral("3MediaA")},
        {QStringLiteral("artist"), QStringLiteral("3MediaB")},
        {QStringLiteral("album"), QStringLiteral("3MediaC")},
        {QStringLiteral("albumArtist"), QStringLiteral("3MediaD")},
        {QStringLiteral("genre"), QStringLiteral("3MediaE")},
        {QStringLiteral("trackNumber"), QStringLiteral("3MediaF")},
        {QStringLiteral("discNumber"), QStringLiteral("3MediaG")},
        {QStringLiteral("releaseYear"), QStringLiteral("3MediaH")},
        {QStringLiteral("duration"), QStringLiteral("3MediaI")},
        {QStringLiteral("sampleRate"), QStringLiteral("3MediaJ")},
        {QStringLiteral("bitRate"), QStringLiteral("3MediaK")},

        // Miscellaneous Data
        {QStringLiteral("originUrl"), QStringLiteral("4MiscA")},
    };
    return g.value(k, QStringLiteral("lastGroup"));
}

QString label(const QString &k)
{
    if (k == QLatin1String("dimensions"))
        return i18ndc("baloowidgets5", "@label", "Dimensions");
    if (k == QLatin1String("gpsLocation"))
        return i18ndc("baloowidgets5", "@label", "GPS Location");
    return PropertyInfo::fromName(k).displayName();
}

QString valueText(const QString &k, const QVariant &v)
{
    if (k == QLatin1String("dimensions"))
        return i18ndc("baloowidgets5", "width × height", "%1 × %2", v.toSize().width(), v.toSize().height());
    if (k == QLatin1String("gpsLocation")) {
        const auto point = v.value<QPair<float, float>>();
        const QString latitude = point.first < 0 ? i18ndc("baloowidgets5", "Latitude (South)", "%1°S", -point.first)
                                                : i18ndc("baloowidgets5", "Latitude (North)", "%1°N", point.first);
        const QString longitude = point.second < 0 ? i18ndc("baloowidgets5", "Longitude (West)", "%1°W", -point.second)
                                                  : i18ndc("baloowidgets5", "Longitude (East)", "%1°E", point.second);
        return latitude + QLatin1Char(' ') + longitude;
    }
    const PropertyInfo pi = PropertyInfo::fromName(k);
    if (pi.valueType() == QMetaType::QDateTime || pi.valueType() == QMetaType::QDate) {
        const QDateTime dt = QDateTime::fromString(v.toString(), Qt::ISODate);
        if (dt.isValid()) {
            KFormat form;
            const QTime t = dt.time();
            if (!t.hour() && !t.minute() && !t.second())
                return form.formatRelativeDate(dt.date(), QLocale::LongFormat);
            return form.formatRelativeDateTime(dt, QLocale::LongFormat);
        }
        return v.toString();
    }
    return pi.formatAsDisplayString(v);
}

QWidget *detailsRows(const QVariantMap &data)
{
    auto meta = new QWidget;
    auto grid = new QGridLayout(meta);
    grid->setContentsMargins(0, 0, 0, 0);
    grid->setSpacing(meta->fontMetrics().height() / 4);
    grid->setColumnStretch(0, 4);
    grid->setColumnStretch(1, 0);
    grid->setColumnStretch(2, 6);
    grid->setColumnStretch(3, 0);

    QMap<QString, QString> order;
    for (auto it = data.constBegin(); it != data.constEnd(); ++it)
        order.insert(group(it.key()) + label(it.key()), it.key());

    const int spacerWidth = QFontMetrics(meta->font()).size(Qt::TextSingleLine, QStringLiteral(" ")).width();
    int row = 0;
    for (const QString &k : std::as_const(order)) {
        auto l = new QLabel(label(k) + QLatin1Char(':'), meta);
        l->setObjectName(QStringLiteral("L_%1").arg(k));
        l->setTextFormat(Qt::PlainText);
        l->setTextInteractionFlags(Qt::TextSelectableByMouse | Qt::TextSelectableByKeyboard);
        l->setSizePolicy(QSizePolicy::MinimumExpanding, QSizePolicy::Preferred);
        l->setWordWrap(true);
        l->setAlignment(Qt::AlignTop | Qt::AlignRight);
        // Fill the cell; QLabel itself aligns its text right. A layout
        // alignment shrinks the wrapped label to sizeHint().width() after
        // the grid computed its height for the wider column, clipping text.
        grid->addWidget(l, row, 0);
        grid->addItem(new QSpacerItem(spacerWidth, 1), row, 1);
        auto v = new ValueWidget(meta);
        v->setObjectName(QStringLiteral("V_%1").arg(k));
        v->setWordWrap(true);
        v->setAlignment(Qt::AlignTop | Qt::AlignLeft);
        v->setTextFormat(Qt::PlainText);
        v->setText(valueText(k, data.value(k)));
        v->setTextInteractionFlags(Qt::TextBrowserInteraction | Qt::TextSelectableByKeyboard);
        grid->addWidget(v, row, 2, Qt::AlignLeft);
        l->setBuddy(v);
        grid->setRowStretch(row, 0);
        ++row;
    }
    grid->addItem(new QSpacerItem(0, 0), row, 0, 1, -1);
    grid->setRowStretch(row, 1);

    auto container = new QWidget;
    auto vbox = new QVBoxLayout(container);
    vbox->setContentsMargins(0, 0, 0, 0);
    vbox->setSpacing(0);
    vbox->addWidget(meta);
    vbox->addStretch(1);

    auto area = new QScrollArea;
    area->setWidget(container);
    area->setFocusProxy(container);
    area->setWidgetResizable(true);
    area->setFrameShape(QFrame::NoFrame);
    area->setObjectName(QStringLiteral("konedrive_details"));
    return area;
}

} // namespace konedrive
