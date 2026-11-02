// Shared wire-contract mapping; neither function opens file content.
#pragma once
#include <KFileMetaData/ExtractionResult>
#include <QByteArray>
#include <QString>
namespace konedrive {
QByteArray readMediaAttribute(const QString &path);
void addMediaFacts(const QByteArray &payload, KFileMetaData::ExtractionResult *result);
}
