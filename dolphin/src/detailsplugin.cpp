// Installed fallback for KDE's slow-mount metadata gate. Do no mount work
// in the plugin constructor: DetailsTab owns the asynchronous children.
#include "detailstab.h"
#include <KPluginFactory>
#include <KPropertiesDialogPlugin>

class KonedriveDetailsPlugin : public KPropertiesDialogPlugin
{
    Q_OBJECT
public:
    KonedriveDetailsPlugin(QObject *parent, const QVariantList &)
        : KPropertiesDialogPlugin(parent)
    {
        new konedrive::DetailsTab(properties, properties->items());
    }
};
K_PLUGIN_CLASS_WITH_JSON(KonedriveDetailsPlugin, "konedrive_detailstab.json")
#include "detailsplugin.moc"
