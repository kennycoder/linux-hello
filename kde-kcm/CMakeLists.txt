cmake_minimum_required(VERSION 3.20)
project(kcm_linuxhello LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(ECM REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

# Determine Qt6 plugin directory path for installation
execute_process(
    COMMAND qtpaths6 --plugin-dir
    OUTPUT_VARIABLE QT6_PLUGIN_DIR
    OUTPUT_STRIP_TRAILING_WHITESPACE
    ERROR_QUIET
)
if(QT6_PLUGIN_DIR)
    file(RELATIVE_PATH REL_PLUGIN_DIR "${CMAKE_INSTALL_PREFIX}" "${QT6_PLUGIN_DIR}")
    if(NOT REL_PLUGIN_DIR MATCHES "^\\.\\.")
        set(KDE_INSTALL_QTPLUGINDIR "${REL_PLUGIN_DIR}" CACHE PATH "Qt plugin directory" FORCE)
        set(KDE_INSTALL_PLUGINDIR "${REL_PLUGIN_DIR}" CACHE PATH "KDE plugin directory" FORCE)
    endif()
endif()

set(QT_MAJOR_VERSION 6)
include(KDEInstallDirs)
include(KDECMakeSettings)

find_package(Qt6 REQUIRED COMPONENTS Core Quick Network)
find_package(KF6 REQUIRED COMPONENTS CoreAddons KCMUtils I18n)

set(CMAKE_LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")

kcmutils_add_qml_kcm(kcm_linuxhello SOURCES kcm_linuxhello.cpp)

target_link_libraries(kcm_linuxhello PRIVATE
    Qt6::Core
    Qt6::Quick
    Qt6::Network
    KF6::CoreAddons
    KF6::KCMUtils
    KF6::KCMUtilsQuick
    KF6::I18n
)
