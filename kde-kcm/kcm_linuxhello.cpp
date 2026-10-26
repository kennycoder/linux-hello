#include "kcm_linuxhello.h"
#include <KPluginFactory>
#include <KLocalizedString>

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLocalSocket>
#include <QProcess>
#include <QStandardPaths>

#include <pwd.h>
#include <unistd.h>

K_PLUGIN_CLASS_WITH_JSON(KcmLinuxHello, "kcm_linuxhello.json")

KcmLinuxHello::KcmLinuxHello(QObject *parent, const KPluginMetaData &metaData)
    : KQuickConfigModule(parent, metaData)
{
    setButtons(NoAdditionalButton);
    detectCurrentUsername();
    refreshStatus();
}

void KcmLinuxHello::detectCurrentUsername()
{
    m_currentUsername = qEnvironmentVariable("USER");
    if (m_currentUsername.isEmpty()) {
        m_currentUsername = qEnvironmentVariable("LOGNAME");
    }
    if (m_currentUsername.isEmpty()) {
        struct passwd *pw = getpwuid(getuid());
        if (pw && pw->pw_name) {
            m_currentUsername = QString::fromLatin1(pw->pw_name);
        }
    }
    if (m_currentUsername.isEmpty()) {
        m_currentUsername = QStringLiteral("user");
    }
}

void KcmLinuxHello::refreshStatus()
{
    m_daemonActive = false;
    m_cameraFound = false;
    m_cameraPath.clear();
    m_enrolledUsers.clear();

    // 1. Try connecting to Unix socket of linux-hellod
    QLocalSocket socket;
    socket.connectToServer(QStringLiteral("/run/linux-hello.sock"));
    if (socket.waitForConnected(400)) {
        socket.write("{\"action\":\"status\"}\n");
        if (socket.waitForBytesWritten(400) && socket.waitForReadyRead(600)) {
            QByteArray line = socket.readLine();
            QJsonDocument doc = QJsonDocument::fromJson(line);
            if (doc.isObject()) {
                QJsonObject obj = doc.object();
                if (obj.value(QStringLiteral("type")).toString() == QLatin1String("status")) {
                    m_daemonActive = true;
                    m_cameraFound = obj.value(QStringLiteral("camera_found")).toBool();
                    m_cameraPath = obj.value(QStringLiteral("camera_path")).toString();
                    const QJsonArray usersArray = obj.value(QStringLiteral("enrolled_users")).toArray();
                    for (const QJsonValue &v : usersArray) {
                        m_enrolledUsers.append(v.toString());
                    }
                }
            }
        }
        socket.disconnectFromServer();
    }

    // 2. Fallback for hardware & models if daemon not responding
    if (!m_daemonActive) {
        // Enrolled models from /var/lib/linux-hello/models
        QDir modelsDir(QStringLiteral("/var/lib/linux-hello/models"));
        if (modelsDir.exists()) {
            const QStringList files = modelsDir.entryList(QStringList{QStringLiteral("*.json")}, QDir::Files);
            for (const QString &f : files) {
                if (f.endsWith(QStringLiteral(".json"))) {
                    m_enrolledUsers.append(f.left(f.length() - 5));
                }
            }
        }

        // Camera detection from /sys/class/video4linux
        QDir v4lDir(QStringLiteral("/sys/class/video4linux"));
        if (v4lDir.exists()) {
            const QStringList entries = v4lDir.entryList(QDir::Dirs | QDir::NoDotAndDotDot);
            for (const QString &entry : entries) {
                QFile nameFile(v4lDir.filePath(entry) + QStringLiteral("/name"));
                if (nameFile.open(QIODevice::ReadOnly | QIODevice::Text)) {
                    QString name = QString::fromUtf8(nameFile.readLine()).trimmed();
                    if (!name.isEmpty()) {
                        m_cameraFound = true;
                        if (name.contains(QStringLiteral("IR"), Qt::CaseInsensitive)) {
                            m_cameraPath = QStringLiteral("/dev/%1 (%2)").arg(entry, name);
                            break; // Prioritize dedicated IR camera
                        } else if (m_cameraPath.isEmpty()) {
                            m_cameraPath = QStringLiteral("/dev/%1 (%2)").arg(entry, name);
                        }
                    }
                }
            }
        }
    }

    m_currentUserEnrolled = m_enrolledUsers.contains(m_currentUsername);

    // 3. PAM status
    m_pamSystemWide = false;
    auto checkPamFile = [](const QString &path) -> bool {
        QFile file(path);
        if (file.open(QIODevice::ReadOnly | QIODevice::Text)) {
            while (!file.atEnd()) {
                QString line = QString::fromUtf8(file.readLine()).trimmed();
                if (!line.startsWith(QLatin1Char('#')) && line.contains(QStringLiteral("pam_linux_hello.so"))) {
                    return true;
                }
            }
        }
        return false;
    };

    if (checkPamFile(QStringLiteral("/etc/pam.d/common-auth")) ||
        checkPamFile(QStringLiteral("/etc/pam.d/system-auth"))) {
        m_pamSystemWide = true;
    }

    m_pamSudoOnly = QFile::exists(QStringLiteral("/etc/pam.d/sudo-hello")) &&
                    (QFile::exists(QStringLiteral("/etc/sudoers.d/linux-hello")) ||
                     QFile::exists(QStringLiteral("/etc/sudoers.d/custom_etc_sudoers_d")));

    computeOverallStatus();
    Q_EMIT statusChanged();
}

void KcmLinuxHello::computeOverallStatus()
{
    const bool pamConfigured = (m_pamSystemWide || m_pamSudoOnly);

    if (!m_daemonActive) {
        m_overallStatus = StatusDaemonStopped;
        m_statusTitle = i18n("Background Service Inactive");
        m_statusDescription = i18n("The linux-hello service is not running. Authentication will fall back to password.");
    } else if (!m_cameraFound) {
        m_overallStatus = StatusNoCamera;
        m_statusTitle = i18n("No Camera Detected");
        m_statusDescription = i18n("No compatible webcam or infrared camera detected on this system.");
    } else if (!m_currentUserEnrolled) {
        m_overallStatus = StatusNotEnrolled;
        m_statusTitle = i18n("Facial Profile Not Enrolled");
        m_statusDescription = i18n("User '%1' has not registered a face profile yet. Open the Manager to enroll.", m_currentUsername);
    } else if (!pamConfigured) {
        m_overallStatus = StatusPamDisabled;
        m_statusTitle = i18n("Authentication Disabled in PAM");
        m_statusDescription = i18n("Biometric facial templates exist, but PAM authentication is not enabled.");
    } else {
        m_overallStatus = StatusActive;
        m_statusTitle = i18n("Enabled & Operational");
        m_statusDescription = i18n("Facial recognition is active for user '%1' and ready for login and sudo authentication.", m_currentUsername);
    }
}

void KcmLinuxHello::launchGui()
{
    QStringList candidates = {
        QStringLiteral("/usr/bin/linux-hello-gui"),
        QStringLiteral("/usr/local/bin/linux-hello-gui"),
        QStandardPaths::findExecutable(QStringLiteral("linux-hello-gui")),
        // In-development workspace fallback
        QStringLiteral("/home/nikolai/Development/linux-hello/target/release/linux-hello-gui")
    };

    QString targetExe;
    for (const QString &cand : candidates) {
        if (!cand.isEmpty() && QFileInfo::exists(cand) && QFileInfo(cand).isExecutable()) {
            targetExe = cand;
            break;
        }
    }

    if (targetExe.isEmpty()) {
        targetExe = QStringLiteral("linux-hello-gui");
    }

    QProcess::startDetached(targetExe, QStringList{});
}

void KcmLinuxHello::startService()
{
    QProcess::startDetached(QStringLiteral("pkexec"), QStringList{
        QStringLiteral("systemctl"),
        QStringLiteral("restart"),
        QStringLiteral("linux-hello.service")
    });
}

#include "kcm_linuxhello.moc"
