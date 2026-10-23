import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.kirigamiaddons.formcard as FormCard
import org.kde.kcmutils as KCMUtils

KCMUtils.SimpleKCM {
    id: root

    title: i18n("Facial Authentication")

    header: Kirigami.InlineMessage {
        id: statusBanner
        visible: true
        type: {
            switch (kcm.overallStatus) {
                case 0: // StatusActive
                    return Kirigami.MessageType.Positive;
                case 1: // StatusNotEnrolled
                    return Kirigami.MessageType.Information;
                case 2: // StatusDaemonStopped
                case 3: // StatusPamDisabled
                case 4: // StatusNoCamera
                default:
                    return Kirigami.MessageType.Warning;
            }
        }
        text: kcm.statusTitle + (kcm.statusDescription !== "" ? ": " + kcm.statusDescription : "")

        actions: [
            Kirigami.Action {
                text: i18n("Refresh")
                icon.name: "view-refresh"
                onTriggered: kcm.refreshStatus()
            }
        ]
    }

    ColumnLayout {
        id: layout
        spacing: 0
        Layout.fillWidth: true

        FormCard.FormHeader {
            title: i18n("Authentication Manager")
        }

        FormCard.FormCard {
            FormCard.FormButtonDelegate {
                id: launchBtn
                icon.name: "camera-web"
                text: i18n("Open Linux Hello Manager")
                description: i18n("Enroll face models, calibrate camera, configure PAM rules and test recognition")
                onClicked: kcm.launchGui()
            }
        }

        FormCard.FormHeader {
            title: i18n("Biometric Profiles")
        }

        FormCard.FormCard {
            FormCard.FormTextDelegate {
                id: currentUserDelegate
                icon.name: "user-identity"
                text: i18n("Current User (%1)", kcm.currentUsername)
                description: kcm.currentUserEnrolled
                    ? i18n("Face model registered and active")
                    : i18n("No face model enrolled for this account")
                trailing: QQC2.Label {
                    text: kcm.currentUserEnrolled ? i18n("Enrolled") : i18n("Not Enrolled")
                    color: kcm.currentUserEnrolled ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.neutralTextColor
                    font.weight: Font.DemiBold
                }
            }

            FormCard.FormDelegateSeparator {
                above: currentUserDelegate
                below: allUsersDelegate
            }

            FormCard.FormTextDelegate {
                id: allUsersDelegate
                icon.name: "system-users"
                text: i18n("Enrolled Accounts")
                description: kcm.enrolledCount > 0
                    ? kcm.enrolledUsers.join(", ")
                    : i18n("No users enrolled on this system")
                trailing: QQC2.Label {
                    text: i18np("%1 user", "%1 users", kcm.enrolledCount)
                    color: Kirigami.Theme.disabledTextColor
                }
            }
        }

        FormCard.FormHeader {
            title: i18n("System & Hardware Status")
        }

        FormCard.FormCard {
            FormCard.FormTextDelegate {
                id: daemonDelegate
                icon.name: kcm.daemonActive ? "emblem-default" : "dialog-warning"
                text: i18n("Background Service (linux-hellod)")
                description: kcm.daemonActive
                    ? i18n("Daemon is active and listening on /run/linux-hello.sock")
                    : i18n("Daemon is stopped or socket is inaccessible")
                trailing: QQC2.Button {
                    visible: !kcm.daemonActive
                    text: i18n("Start Daemon")
                    icon.name: "system-run"
                    onClicked: kcm.startService()
                }
            }

            FormCard.FormDelegateSeparator {
                above: daemonDelegate
                below: cameraDelegate
            }

            FormCard.FormTextDelegate {
                id: cameraDelegate
                icon.name: kcm.cameraFound ? "camera-web" : "camera-web-symbolic-missing"
                text: i18n("Camera Hardware")
                description: kcm.cameraFound
                    ? (kcm.cameraPath !== "" ? kcm.cameraPath : i18n("Compatible camera device detected"))
                    : i18n("No camera devices found in /sys/class/video4linux")
                trailing: QQC2.Label {
                    text: kcm.cameraFound ? i18n("Detected") : i18n("Missing")
                    color: kcm.cameraFound ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.negativeTextColor
                }
            }

            FormCard.FormDelegateSeparator {
                above: cameraDelegate
                below: pamDelegate
            }

            FormCard.FormTextDelegate {
                id: pamDelegate
                icon.name: (kcm.pamSystemWide || kcm.pamSudoOnly) ? "security-high" : "security-low"
                text: i18n("PAM Integration")
                description: kcm.pamSystemWide
                    ? i18n("System-wide PAM enabled in /etc/pam.d/common-auth (login, lockscreen, sudo)")
                    : (kcm.pamSudoOnly
                        ? i18n("Sudo PAM enabled in /etc/pam.d/sudo-hello")
                        : i18n("Not enabled in system PAM configuration"))
                trailing: QQC2.Label {
                    text: kcm.pamSystemWide ? i18n("System-wide") : (kcm.pamSudoOnly ? i18n("Sudo Only") : i18n("Disabled"))
                    color: (kcm.pamSystemWide || kcm.pamSudoOnly) ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.neutralTextColor
                }
            }
        }
    }
}

