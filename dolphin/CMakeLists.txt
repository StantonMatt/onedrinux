cmake_minimum_required(VERSION 3.24)
# A placeholder, shown nowhere; the packages' version is Cargo's (docs/releasing.md).
project(konedrive-dolphin VERSION 0.1.0 LANGUAGES CXX)

# Dolphin integration: an overlay emblem for each file's and folder's state and pin, and
# "Always keep on this device" / "Free up space" / "View online" in the context menu. Neither
# plugin ever opens a file in the sync folder -- reading one that is not on
# this computer downloads it.

set(QT_MIN_VERSION "6.8.0")
# Below 6.21 KIO's "Move to Trash" copies the file into the home trash, which downloads it
# (docs/limitations/K34.md).
set(KF_MIN_VERSION "6.21.0")

find_package(ECM ${KF_MIN_VERSION} REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)

find_package(Qt6 ${QT_MIN_VERSION} REQUIRED COMPONENTS Core Gui Widgets DBus)
find_package(KF6 ${KF_MIN_VERSION} REQUIRED COMPONENTS KIO CoreAddons I18n)

add_definitions(-DTRANSLATION_DOMAIN=\"konedrive_dolphin\")

# Everything the two plugins decide, in one library the tests link directly.
add_library(konedrive_dolphin_core STATIC
    src/filestate.cpp
    src/generated/refusaltexts.cpp
    src/overlayengine.cpp
    src/refusaltext.cpp
    src/syncclient.cpp)
set_target_properties(konedrive_dolphin_core PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_include_directories(konedrive_dolphin_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/src)
target_link_libraries(konedrive_dolphin_core PUBLIC Qt6::Core Qt6::DBus KF6::I18n)

kcoreaddons_add_plugin(konedriveoverlay SOURCES src/overlayplugin.cpp INSTALL_NAMESPACE "kf6/overlayicon")
target_link_libraries(konedriveoverlay PRIVATE konedrive_dolphin_core KF6::KIOCore)

kcoreaddons_add_plugin(konedriveactions SOURCES src/actionplugin.cpp INSTALL_NAMESPACE "kf6/kfileitemaction")
target_link_libraries(konedriveactions PRIVATE konedrive_dolphin_core KF6::KIOWidgets KF6::CoreAddons KF6::I18n Qt6::Widgets)

# The emblems, including all syncing frames (emblems/generate.py), in hicolor,
# which every icon theme falls back on. Each size installs the same icon names.
# KIconLoader draws an emblem at 8 px on a 16-22 px icon (every icon of the Details
# view), at 16 px up to 48, 22 px up to 96 and 32 px above, times the screen's scale.
# hicolor has no 8x8 directory, so the 8 px set is the scalable one: at scale 1 only
# 8 px reaches it, every other size having a directory of its own -- 16, 22 and 32 px,
# the same at scale 2 (@2, drawn from the 32 px set), and 48, 64 and 96 px for scale 3.
install(DIRECTORY emblems/8/ DESTINATION ${KDE_INSTALL_ICONDIR}/hicolor/scalable/emblems COMPONENT emblems)
foreach(size 16 22 32)
    install(DIRECTORY emblems/${size}/ DESTINATION ${KDE_INSTALL_ICONDIR}/hicolor/${size}x${size}/emblems COMPONENT emblems)
    install(DIRECTORY emblems/32/ DESTINATION ${KDE_INSTALL_ICONDIR}/hicolor/${size}x${size}@2/emblems COMPONENT emblems)
endforeach()
foreach(size 48 64 96)
    install(DIRECTORY emblems/32/ DESTINATION ${KDE_INSTALL_ICONDIR}/hicolor/${size}x${size}/emblems COMPONENT emblems)
endforeach()

if(BUILD_TESTING)
    add_subdirectory(tests)
endif()
