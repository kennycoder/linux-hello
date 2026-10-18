use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use linux_hello_camera::CameraStream;
use linux_hello_core::{Face, FaceEngine};

#[derive(Debug, Clone)]
pub struct FramePacket {
    pub width: u32,
    pub height: u32,
    pub gray_data: Vec<u8>,
    pub mean_brightness: f32,
    pub faces: Vec<Face>,
    pub inference_ms: f32,
    pub fps: f32,
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Started,
    Frame(FramePacket),
    Error(String),
    Stopped,
}

pub struct CameraWorker {
    stop_flag: Arc<AtomicBool>,
    rx: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
}

impl CameraWorker {
    pub fn start(
        device_path: PathBuf,
        models_dir: PathBuf,
        ctx: eframe::egui::Context,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);
        let (tx, rx) = mpsc::sync_channel::<WorkerEvent>(2);

        let handle = thread::spawn(move || {
            let det_model = models_dir.join("face_detection_yunet.onnx");
            let rec_model = models_dir.join("face_recognition_sface.onnx");

            let mut stream = match CameraStream::open(&device_path, 640, 400) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(WorkerEvent::Error(format!(
                        "Failed to open camera {:?}: {}",
                        device_path, e
                    )));
                    ctx.request_repaint();
                    return;
                }
            };

            let engine = match FaceEngine::new(&det_model, &rec_model, stream.width, stream.height) {
                Ok(eng) => eng,
                Err(e) => {
                    let _ = tx.send(WorkerEvent::Error(format!(
                        "Failed to initialize ONNX face engine: {}",
                        e
                    )));
                    ctx.request_repaint();
                    return;
                }
            };

            let _ = tx.send(WorkerEvent::Started);
            ctx.request_repaint();

            let start_time = Instant::now();
            let mut frames_count = 0u64;

            while !stop_clone.load(Ordering::Relaxed) {
                let frame = match stream.capture_frame(400) {
                    Ok(f) => f,
                    Err(_) => continue,
                };

                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }

                frames_count += 1;
                let elapsed = start_time.elapsed().as_secs_f32();
                let fps = if elapsed > 0.05 {
                    frames_count as f32 / elapsed
                } else {
                    0.0
                };

                let inf_start = Instant::now();
                let faces = if frame.mean_brightness >= 5.0 {
                    engine
                        .process_frame(&frame.gray_data, frame.width, frame.height)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let inference_ms = inf_start.elapsed().as_secs_f32() * 1000.0;

                let packet = FramePacket {
                    width: frame.width,
                    height: frame.height,
                    gray_data: frame.gray_data,
                    mean_brightness: frame.mean_brightness,
                    faces,
                    inference_ms,
                    fps,
                };

                match tx.try_send(WorkerEvent::Frame(packet)) {
                    Ok(()) => {
                        ctx.request_repaint();
                    }
                    Err(TrySendError::Full(_)) => {
                        // UI is busy rendering; drop frame to keep latency zero
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }

            let _ = tx.try_send(WorkerEvent::Stopped);
            ctx.request_repaint();
        });

        Self {
            stop_flag,
            rx,
            handle: Some(handle),
        }
    }

    pub fn poll_events(&self) -> Vec<WorkerEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            events.push(ev);
        }
        events
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CameraWorker {
    fn drop(&mut self) {
        self.stop();
    }
}
