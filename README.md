<p align="center">
  <img src="assets/linux-hello-logo-cropped.svg" alt="Linux-Hello Logo" width="420">
</p>

<p align="center">
  <a href="https://github.com/kennycoder/linux-hello/actions/workflows/ci.yml"><img src="https://github.com/kennycoder/linux-hello/actions/workflows/ci.yml/badge.svg" alt="CI Build & Tests"></a>
  <a href="https://github.com/kennycoder/linux-hello/actions/workflows/release.yml"><img src="https://github.com/kennycoder/linux-hello/actions/workflows/release.yml/badge.svg" alt="Release Package"></a>
  <a href="https://github.com/kennycoder/linux-hello/releases"><img src="https://img.shields.io/github/v/release/kennycoder/linux-hello?include_prereleases&color=blue" alt="Latest Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License: Apache-2.0"></a>
</p>

# Linux-Hello

A high-performance, secure biometric facial authentication system for Linux, designed as a fast, native equivalent to **Windows Hello**.

Built in **Rust** with C++ OpenCV DNN acceleration, leveraging dedicated Infrared (IR) cameras, ONNX face detection & recognition models, and native Debian PAM integration.

---

## Features

- **Dedicated IR Camera Support**:
  - Direct Linux V4L2 `mmap` streaming on `/dev/video{n}` (usually 2, but it also auto-detects IR camera).
  - Automatically filters illuminated infrared strobe frames from ambient/dark frames.
  - **Hardware Safeguard**: Strictly verifies dedicated IR camera capabilities (via `GREY`, `Y16`, `Z16` formats or IR device card metadata). Never defaults to a standard RGB webcam (`/dev/video0`) to prevent insecure fallback.
- **Modern Face Biometrics**: Powered by OpenCV Zoo ONNX models:
  - **YuNet**: Ultra-fast face and 5-point facial landmark detector running at high confidence ($\ge 0.70$).
  - **SFace**: Canonical 112×112 face alignment and 128-dimensional hyperspherical feature embedding extractor with Cosine distance matching.
- **Anti-Spoofing & Anatomical Landmark Validation**:
  - Eliminates false positives from non-face textures, fabric folds, cushions, or arbitrary objects.
  - Validates human anatomical geometry: inter-ocular eye distance ($\ge 18\text{px}$), mouth below eye line ($\ge 8\text{px}$), nose tip vertically positioned between eyes and mouth, bounding box bounds ($\ge 45\times 45\text{px}$), and human facial aspect ratios ($0.45 \le AR \le 1.60$).
  - Strict cosine similarity matching threshold of **0.65** (real, unoccluded faces score $0.70 - 0.90+$, occlusions, covered eyes/features, non-matching faces and noise score $< 0.60$).
- **Interactive Multi-Angle Enrollment**:
  - Guided 5-pose registration state machine: **Center**, **Turn Left** (15-20°), **Turn Right** (15-20°), **Tilt Up** (10-15°), and **Tilt Down** (10-15°).
  - Live horizontal (`yaw`) and vertical (`pitch`) pose estimation with real-time on-screen guidance (e.g. `➔ Turn more to your LEFT`).
  - **3-Frame Steady Hold Requirement**: Verifies that the user holds the target pose steadily (`[●○○]` $\to$ `[●●○]` $\to$ `[●●●]`) before capturing.
- **Client-Daemon Architecture (like `fprintd`)**:
  - `linux-hellod`: Background system daemon keeping models warm in memory for instant sub-100ms verification.
  - `pam_linux_hello.so`: Ultra-lightweight (517 KB) PAM module with zero heavy runtime dependencies in PAM process space.
  - Fail-safe fallback: If camera or daemon is offline, PAM immediately and seamlessly falls back to password authentication without hanging.
- **Debian PAM Integration**: Uses Debian's standard `pam-auth-update` profile mechanism (`/usr/share/pam-configs/linux-hello`), avoiding fragile manual edits to `/etc/pam.d/common-auth`.
- **CLI Goodies**: Complete management tool for enrollment, testing, camera diagnostics, offline image analysis, and service control.

---

## Architecture Overview

```
                 +---------------------------+
                 |  Login / Sudo / GDM / PAM |
                 +-------------+-------------+
                               | (C ABI)
                 +-------------v-------------+
                 |    pam_linux_hello.so     |
                 +-------------+-------------+
                               | Unix Socket (/run/linux-hello.sock)
+-------------+  +-------------v-------------+
| linux-hello |->|       linux-hellod        |
|     CLI     |  +-------------+-------------+
+-------------+                |
                 +-------------+-------------+
                 |   V4L2 IR Stream Capture  |
                 |      (/dev/video2)        |
                 +-------------+-------------+
                               |
                 +-------------v-------------+
                 |     YuNet Face Detector   |
                 |   (Anatomical Validation) |
                 +-------------+-------------+
                               | (5-point alignment)
                 +-------------v-------------+
                 |      SFace Recognizer     |
                 |      (128-D Embedding)    |
                 +-------------+-------------+
                               |
                 +-------------v-------------+
                 |   Cosine Similarity (0.50)|
                 |  vs Enrolled Vector DB    |
                 +---------------------------+
```

---

## Directory Structure

```
linux-hello/
├── crates/
│   ├── linux-hello-camera/    # Direct V4L2 ioctl streaming & IR camera discovery
│   ├── linux-hello-core/      # OpenCV YuNet + SFace C++ bridge & Rust safe API
│   ├── linux-hellod/          # Background daemon service & Unix socket IPC
│   ├── pam_linux_hello/       # Thin PAM module (.so) with password fallback
│   └── linux-hello-cli/       # CLI utility (linux-hello)
├── models/
│   ├── face_detection_yunet.onnx
│   └── face_recognition_sface.onnx
├── debian/
│   ├── linux-hello.service    # Systemd service unit
│   └── pam-config             # pam-auth-update integration profile
├── Makefile                   # Build and system installation rules
└── README.md
```

---

## Prerequisites & Build Dependencies

When building from source, `linux-hello-core` compiles a C++17 bridge against **OpenCV 4** (`opencv_core`, `opencv_dnn`, `opencv_objdetect`, `opencv_imgproc`) to accelerate YuNet (`cv::FaceDetectorYN`) and SFace (`cv::FaceRecognizerSF`) ONNX inference.

Install the required system dependencies for your distribution:

- **Debian / Ubuntu**:
  ```bash
  sudo apt update
  sudo apt install build-essential libopencv-dev libpam0g-dev
  ```
- **RHEL / Fedora / Rocky / AlmaLinux**:
  ```bash
  sudo dnf install gcc-c++ make opencv-devel pam-devel
  ```
- **Arch Linux**:
  ```bash
  sudo pacman -S --needed base-devel opencv pam
  ```

You also need a **Rust toolchain** (Rust 1.85+ for the 2024 edition, via [rustup.rs](https://rustup.rs/) or your system package manager). Verify all build prerequisites at any time with:

```bash
make check-deps
```

---

## Quick Start

### 1. Build
```bash
make
```
*(To clean build artifacts at any time, run `make clean`.)*

### 2. Inspect Connected Cameras
```bash
./target/release/linux-hello cameras
```
Output:
```text
Detecting connected video devices...

Device: "/dev/video2" [IR CAMERA]
  Card:    ASUS FHD webcam: ASUS IR camera
  Driver:  uvcvideo
  Formats: GREY
```

### 3. Take a Snapshot from the IR Camera
```bash
./target/release/linux-hello snapshot --output test_ir.png
```

### 4. Enroll Your Face (Guided Multi-Angle)
Position yourself in front of the IR camera and run:
```bash
sudo ./target/release/linux-hello add
```
This launches the interactive 5-pose enrollment state machine:
1. **Center**: `Look straight into the IR camera`
2. **Turn Left**: `Turn your head slightly to your LEFT (15-20°)`
3. **Turn Right**: `Turn your head slightly to your RIGHT (15-20°)`
4. **Tilt Up**: `Tilt your head slightly UP (10-15°)`
5. **Tilt Down**: `Tilt your head slightly DOWN (10-15°)`

The CLI displays real-time visual feedback and a 3-frame steady hold meter (`[●○○]` $\to$ `[●●○]` $\to$ `[●●●]`). Once verified, it captures and embeds the angle.

*(Optional flags)*:
- `-r`, `--replace`: Clear any existing models for the user before enrolling.
- `--user <name>`: Enroll for a specific user account.
- `--device /dev/videoX`: Specify a custom camera device node.
- `[LABEL]`: Enroll a single custom template (e.g. `linux-hello add glasses`).

### 5. List Enrolled Profiles
```bash
sudo ./target/release/linux-hello list
```

### 6. Test Live Recognition
```bash
./target/release/linux-hello test
```

---

## Standalone Testing & Debugging (No System Integration Required)

You can thoroughly test and debug face detection, pose estimation, and face recognition models directly without running the daemon, without PAM, and without root:

### 1. Live Camera Stream Debugger (`debug`)
Inspect incoming camera frames, face detection confidence, bounding boxes, 5 facial landmarks, estimated head pose, and cosine similarity scores in real time:
```bash
# Debug face detection and head pose live from camera (30 seconds):
./target/release/linux-hello debug

# Debug matching in real time against your enrolled profile:
./target/release/linux-hello debug --user $USER

# Run indefinitely until Ctrl+C:
./target/release/linux-hello debug --duration 0
```
Live output example:
```text
[0042] Bright: 38.2 | Conf: 0.94 Box: [220,140,150,190] | Pose: yaw=0.51 pitch=0.54 (Facing Center) (32.1ms) | MATCH: 0.742 ('center')
```

### 2. Offline Image Face Inspection (`debug --image`)
Analyze any image file (PNG) to inspect detected faces, bounding boxes, landmarks, confidence, and pose estimation:
```bash
./target/release/linux-hello debug --image test_ir.png
```
Output:
```text
=======================================================
           Linux-Hello Face Debugger (Image)           
=======================================================
Image file:  "test_ir.png"
Dimensions:  640x400
Inference:   36.94ms | Faces detected: 1

[Face #1]
  Confidence:  0.93
  BoundingBox: [x: 223.7, y: 147.5, w: 148.9, h: 191.3]
  Pose Ratios: yaw: 0.51, pitch: 0.55 (Facing Center)
  Landmarks:   R-Eye (267.1,218.1) | L-Eye (333.5,218.5) | Nose (300.9,254.1)
=======================================================
```

### 3. Compare Two Face Images (`compare`)
Completely offline, 1-to-1 facial verification between any two PNG images:
```bash
./target/release/linux-hello compare photo1.png photo2.png
```
Prints face bounding boxes, poses, detection scores for both images, and the exact cosine similarity distance (e.g. `0.785 >= 0.500` -> `[MATCH SUCCESS]`).

### 4. Test an Image Against Enrolled Templates (`identify`)
Identify whether an offline image matches an enrolled user profile:
```bash
./target/release/linux-hello identify photo.png --user $USER
```
Displays individual similarity scores for all enrolled head angles (Center, Left, Right, Up, Down).

### 5. Direct Authentication Test (`test --direct`)
Authenticate directly against camera hardware and local profile storage, completely bypassing the system daemon (`linux-hellod`) and socket:
```bash
./target/release/linux-hello test --direct
```

---

## System Installation & PAM Setup

### Option A: Install from Debian Package (.deb) — Debian / Ubuntu

Download the latest `.deb` package from [GitHub Releases](https://github.com/kennycoder/linux-hello/releases) or build it locally with `make deb`:

```bash
# Install package (apt automatically resolves all runtime dependencies, including OpenCV runtime libs)
sudo apt install ./linux-hello_*_amd64.deb
```

The Debian package automatically:
- Installs `linux-hello`, `linux-hellod`, and `pam_linux_hello.so`.
- Installs the bundled YuNet and SFace ONNX models (`models/*.onnx`) into `/usr/share/linux-hello/models` (no network download required during install).
- Enables and starts `linux-hello.service` in systemd.
- Registers the PAM authentication profile with Debian's `pam-auth-update`.

### Option B: Build and Install from Source (Debian/Ubuntu, RHEL/Fedora, Arch Linux)

```bash
# 1. Install build dependencies for your distro:
# Debian / Ubuntu:
sudo apt install build-essential libopencv-dev libpam0g-dev
# RHEL / Fedora:
sudo dnf install gcc-c++ make opencv-devel pam-devel
# Arch Linux:
sudo pacman -S --needed base-devel opencv pam

# 2. Build and install binaries, PAM module, ONNX models, and systemd service
make
sudo make install

# 3. Start and enable the daemon service
sudo systemctl daemon-reload
sudo systemctl enable --now linux-hello.service
```

#### Enabling PAM Authentication:
- **Debian / Ubuntu** (via `pam-auth-update`):
  ```bash
  sudo linux-hello enable
  # Or to disable at any time:
  sudo linux-hello disable
  ```
- **RHEL / Fedora & Arch Linux**:
  Add the following line near the top of `/etc/pam.d/sudo` (or `/etc/pam.d/system-auth` / `/etc/pam.d/system-local-login`):
  ```text
  auth    sufficient    pam_linux_hello.so
  ```


---

## Verification & Safe Testing

1. Open a separate terminal window and keep a root shell open (`sudo -s`) as a safeguard.
2. In another terminal, run:
   ```bash
   sudo whoami
   ```
   The camera will illuminate and authenticate your face without requiring a password.
3. **Password Fallback**: Cover the IR camera with your hand or a card and run `sudo whoami`. The system will seamlessly fall back to the standard password prompt without hanging.
