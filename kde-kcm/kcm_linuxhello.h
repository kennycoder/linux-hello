#pragma once

#include <KQuickConfigModule>
#include <QStringList>

class KcmLinuxHello : public KQuickConfigModule
{
    Q_OBJECT

    Q_PROPERTY(QString currentUsername READ currentUsername CONSTANT)
    Q_PROPERTY(bool currentUserEnrolled READ isCurrentUserEnrolled NOTIFY statusChanged)
    Q_PROPERTY(QStringList enrolledUsers READ enrolledUsers NOTIFY statusChanged)
    Q_PROPERTY(int enrolledCount READ enrolledCount NOTIFY statusChanged)
    Q_PROPERTY(bool daemonActive READ isDaemonActive NOTIFY statusChanged)
    Q_PROPERTY(bool cameraFound READ isCameraFound NOTIFY statusChanged)
    Q_PROPERTY(QString cameraPath READ cameraPath NOTIFY statusChanged)
    Q_PROPERTY(bool pamSystemWide READ isPamSystemWide NOTIFY statusChanged)
    Q_PROPERTY(bool pamSudoOnly READ isPamSudoOnly NOTIFY statusChanged)
    Q_PROPERTY(int overallStatus READ overallStatus NOTIFY statusChanged)
    Q_PROPERTY(QString statusTitle READ statusTitle NOTIFY statusChanged)
    Q_PROPERTY(QString statusDescription READ statusDescription NOTIFY statusChanged)

public:
    enum StatusType {
        StatusActive = 0,
        StatusNotEnrolled = 1,
        StatusDaemonStopped = 2,
        StatusPamDisabled = 3,
        StatusNoCamera = 4
    };
    Q_ENUM(StatusType)

    explicit KcmLinuxHello(QObject *parent, const KPluginMetaData &metaData);
    ~KcmLinuxHello() override = default;

    QString currentUsername() const { return m_currentUsername; }
    bool isCurrentUserEnrolled() const { return m_currentUserEnrolled; }
    QStringList enrolledUsers() const { return m_enrolledUsers; }
    int enrolledCount() const { return m_enrolledUsers.size(); }
    bool isDaemonActive() const { return m_daemonActive; }
    bool isCameraFound() const { return m_cameraFound; }
    QString cameraPath() const { return m_cameraPath; }
    bool isPamSystemWide() const { return m_pamSystemWide; }
    bool isPamSudoOnly() const { return m_pamSudoOnly; }
    int overallStatus() const { return static_cast<int>(m_overallStatus); }
    QString statusTitle() const { return m_statusTitle; }
    QString statusDescription() const { return m_statusDescription; }

    Q_INVOKABLE void refreshStatus();
    Q_INVOKABLE void launchGui();
    Q_INVOKABLE void startService();

Q_SIGNALS:
    void statusChanged();

private:
    void detectCurrentUsername();
    void computeOverallStatus();

    QString m_currentUsername;
    bool m_currentUserEnrolled = false;
    QStringList m_enrolledUsers;
    bool m_daemonActive = false;
    bool m_cameraFound = false;
    QString m_cameraPath;
    bool m_pamSystemWide = false;
    bool m_pamSudoOnly = false;
    StatusType m_overallStatus = StatusDaemonStopped;
    QString m_statusTitle;
    QString m_statusDescription;
};

