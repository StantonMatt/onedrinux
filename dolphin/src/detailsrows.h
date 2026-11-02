// Baloo-compatible row names, formatting, visibility and layout.
#pragma once
#include <KFileMetaData/Properties>
#include <QVariantMap>
class QWidget;
namespace konedrive {
QVariantMap visibleDetails(const KFileMetaData::PropertyMultiMap &properties);
QWidget *detailsRows(const QVariantMap &data);
}
