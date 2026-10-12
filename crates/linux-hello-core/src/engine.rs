use std::ffi::CString;
use std::path::Path;
use thiserror::Error;

use crate::ffi::*;

#[derive(Error, Debug)]
pub enum EngineError {
    #[error("Failed to load face detection or recognition model: {0}")]
    ModelLoad(String),
    #[error("Internal face processing error occurred")]
    ProcessingError,
    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

#[derive(Debug, Clone)]
pub struct Face {
    pub bbox: (f32, f32, f32, f32), // (x, y, width, height)
    pub score: f32,
    pub landmarks: [(f32, f32); 5],
    pub embedding: Vec<f32>,
    pub eye_contrast: (f32, f32), // (right_eye_std, left_eye_std)
    pub eye_range: (f32, f32),    // (right_eye_range, left_eye_range)
    pub eyes_visible: bool,
}

impl Face {
    /// Returns approximate (yaw, pitch) pose ratios:
    /// yaw: ~0.50 center, >0.57 turned left (user's left), <0.43 turned right (user's right)
    /// pitch: ~0.50 center, <0.43 tilted up, >0.57 tilted down
    pub fn estimated_pose_ratios(&self) -> (f32, f32) {
        let right_eye = self.landmarks[0];
        let left_eye = self.landmarks[1];
        let nose = self.landmarks[2];
        let right_mouth = self.landmarks[3];
        let left_mouth = self.landmarks[4];

        let min_eye_x = right_eye.0.min(left_eye.0);
        let max_eye_x = right_eye.0.max(left_eye.0);
        let eye_dist = max_eye_x - min_eye_x;
        let yaw = if eye_dist > 1.0 {
            (nose.0 - min_eye_x) / eye_dist
        } else {
            0.5
        };

        let eye_mid_y = (right_eye.1 + left_eye.1) / 2.0;
        let mouth_mid_y = (right_mouth.1 + left_mouth.1) / 2.0;
        let vert_dist = (mouth_mid_y - eye_mid_y).abs();
        let pitch = if vert_dist > 1.0 {
            (nose.1 - eye_mid_y) / vert_dist
        } else {
            0.5
        };

        (yaw, pitch)
    }

    /// Verifies that detected face satisfies basic human anatomical geometry and high confidence,
    /// preventing non-face textures (pillows, fabric folds, reflections) from being treated as faces.
    pub fn is_valid_human_face(&self) -> bool {
        // 1. Detection confidence threshold: must be >= 0.65
        if self.score < 0.65 {
            return false;
        }

        // 2. Minimum bounding box: face must be at least 36x36 pixels (supports larger sitting distance)
        if self.bbox.2 < 36.0 || self.bbox.3 < 36.0 {
            return false;
        }

        // 3. Aspect ratio: width/height between 0.45 and 1.60
        let ar = self.bbox.2 / self.bbox.3;
        if !(0.45..=1.60).contains(&ar) {
            return false;
        }

        // 4. Landmark geometry:
        let right_eye = self.landmarks[0];
        let left_eye = self.landmarks[1];
        let nose = self.landmarks[2];
        let right_mouth = self.landmarks[3];
        let left_mouth = self.landmarks[4];

        // Eye distance must be at least 12 pixels (supports larger distance and yaw turns)
        let eye_dist = (left_eye.0 - right_eye.0).hypot(left_eye.1 - right_eye.1);
        if eye_dist < 12.0 {
            return false;
        }

        // Mouth corners must be below eyes
        let eye_y = (right_eye.1 + left_eye.1) / 2.0;
        let mouth_y = (right_mouth.1 + left_mouth.1) / 2.0;
        if mouth_y <= eye_y + 6.0 {
            return false;
        }

        // Nose tip must be roughly between eyes and mouth in vertical axis
        if nose.1 < eye_y - 5.0 || nose.1 > mouth_y + 15.0 {
            return false;
        }

        // 5. Eye visibility & unoccluded check:
        // Rejects faces with eyes covered by fingers, hands, masks, or sleeping/closed eyes
        if !self.eyes_visible {
            return false;
        }

        true
    }

    /// Checks whether the face angle matches the requested target pose.
    /// Returns (matched, guidance_instruction).
    pub fn matches_target_pose(&self, target_pose: &str) -> (bool, &'static str) {
        self.matches_target_pose_with_baseline(target_pose, None)
    }

    /// Checks whether the face angle matches the requested target pose, optionally
    /// accounting for the user's natural neutral "center" pose baseline `(center_yaw, center_pitch)`.
    pub fn matches_target_pose_with_baseline(
        &self,
        target_pose: &str,
        center_baseline: Option<(f32, f32)>,
    ) -> (bool, &'static str) {
        let (yaw, pitch) = self.estimated_pose_ratios();
        match target_pose {
            "center" => {
                if yaw < 0.38 {
                    (false, "Turn slightly to your LEFT (towards center)")
                } else if yaw > 0.62 {
                    (false, "Turn slightly to your RIGHT (towards center)")
                } else if pitch < 0.40 {
                    (false, "Tilt head slightly DOWN (towards center)")
                } else if pitch > 0.65 {
                    (false, "Tilt head slightly UP (towards center)")
                } else {
                    (true, "Holding Center")
                }
            }
            "look_left" => {
                let rel_match = center_baseline.map_or(false, |(cy, _)| yaw >= cy + 0.04);
                if yaw >= 0.54 || rel_match {
                    (true, "Turned Left")
                } else {
                    (false, "Turn slightly more to your LEFT")
                }
            }
            "look_right" => {
                let rel_match = center_baseline.map_or(false, |(cy, _)| yaw <= cy - 0.04);
                if yaw <= 0.46 || rel_match {
                    (true, "Turned Right")
                } else {
                    (false, "Turn slightly more to your RIGHT")
                }
            }
            "look_up" => {
                let rel_match = center_baseline.map_or(false, |(_, cp)| pitch <= cp - 0.03);
                if pitch <= 0.53 || rel_match {
                    (true, "Tilted Up")
                } else {
                    (false, "Tilt head slightly higher UP")
                }
            }
            "look_down" => {
                let rel_match = center_baseline.map_or(false, |(_, cp)| pitch >= cp + 0.03);
                if pitch >= 0.55 || rel_match {
                    (true, "Tilted Down")
                } else {
                    (false, "Tilt head slightly further DOWN")
                }
            }
            _ => (true, "Valid Pose"),
        }
    }
}

pub struct FaceEngine {
    ptr: *mut FaceEngineOpaque,
}

unsafe impl Send for FaceEngine {}
unsafe impl Sync for FaceEngine {}

impl FaceEngine {
    pub fn new(det_path: &Path, rec_path: &Path, width: u32, height: u32) -> Result<Self, EngineError> {
        let det_cstr = CString::new(det_path.to_string_lossy().as_bytes())
            .map_err(|_| EngineError::InvalidPath(det_path.to_string_lossy().into()))?;
        let rec_cstr = CString::new(rec_path.to_string_lossy().as_bytes())
            .map_err(|_| EngineError::InvalidPath(rec_path.to_string_lossy().into()))?;

        let ptr = unsafe {
            face_engine_new(
                det_cstr.as_ptr(),
                rec_cstr.as_ptr(),
                width as i32,
                height as i32,
            )
        };

        if ptr.is_null() {
            return Err(EngineError::ModelLoad(format!(
                "Detector: {:?}, Recognizer: {:?}",
                det_path, rec_path
            )));
        }

        Ok(FaceEngine { ptr })
    }

    pub fn process_frame(&self, gray_data: &[u8], width: u32, height: u32) -> Result<Vec<Face>, EngineError> {
        const MAX_FACES: usize = 10;
        let mut raw_faces = [RawFace::default(); MAX_FACES];

        let count = unsafe {
            face_engine_detect_and_embed(
                self.ptr,
                gray_data.as_ptr(),
                width as i32,
                height as i32,
                raw_faces.as_mut_ptr(),
                MAX_FACES as i32,
            )
        };

        if count < 0 {
            return Err(EngineError::ProcessingError);
        }

        let mut faces = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let rf = &raw_faces[i];
            let mut landmarks = [(0.0, 0.0); 5];
            for l in 0..5 {
                landmarks[l] = (rf.landmarks[l * 2], rf.landmarks[l * 2 + 1]);
            }

            let eye_dist = (landmarks[1].0 - landmarks[0].0).hypot(landmarks[1].1 - landmarks[0].1);
            let (r_std, r_range) = Self::compute_eye_metrics(gray_data, width, height, landmarks[0].0, landmarks[0].1, eye_dist);
            let (l_std, l_range) = Self::compute_eye_metrics(gray_data, width, height, landmarks[1].0, landmarks[1].1, eye_dist);

            // Open/visible eyes in IR exhibit a dark iris/pupil core (basin) surrounded
            // horizontally by brighter sclera/canthus and vertically by a brighter lower eyelid,
            // along with strong horizontal scanline variance across the aperture.
            // When the head turns 15-25+ degrees left/right, the far eye is foreshortened and
            // partially shadowed by the nose bridge while the near eye remains clearly visible.
            let eye_dx = (landmarks[1].0 - landmarks[0].0).max(1.0);
            let yaw_ratio = ((landmarks[2].0 - landmarks[0].0) / eye_dx).clamp(0.0, 1.0);
            let yaw_dev = (yaw_ratio - 0.50).abs();
            let turn_relax = (1.0 - (yaw_dev - 0.05).max(0.0) * 1.8).clamp(0.55, 1.0);

            let dist_scale = (eye_dist / 42.0).clamp(0.75, 1.0);
            let min_std = r_std.min(l_std);
            let max_std = r_std.max(l_std);
            let avg_std = (r_std + l_std) / 2.0;
            let min_range = r_range.min(l_range);

            let eyes_visible = min_std >= 8.8 * dist_scale * turn_relax
                && max_std >= 11.2 * dist_scale
                && avg_std >= 11.2 * dist_scale * turn_relax.sqrt()
                && min_range >= 18.0 * dist_scale * turn_relax;

            faces.push(Face {
                bbox: (rf.x, rf.y, rf.width, rf.height),
                score: rf.score,
                landmarks,
                embedding: rf.embedding.to_vec(),
                eye_contrast: (r_std, l_std),
                eye_range: (r_range, l_range),
                eyes_visible,
            });
        }

        Ok(faces)
    }

    /// Computes an illumination-normalized, gaze/turn-invariant eye-openness contrast score
    /// and dynamic range around an eye landmark. Open eyes in IR have a dark iris/pupil core
    /// surrounded horizontally by brighter sclera/skin and below by a brighter lower eyelid,
    /// plus high within-row horizontal variance. Sampling across [-0.14*ed .. +0.14*ed] allows
    /// the iris to shift towards the eye corners during 15-25° left/right head turns without
    /// losing contrast, while per-column vertical dip comparison cancels out horizontal shading
    /// on closed eyelids.
    pub fn compute_eye_metrics(
        gray_data: &[u8],
        width: u32,
        height: u32,
        eye_x: f32,
        eye_y: f32,
        eye_dist: f32,
    ) -> (f32, f32) {
        if width == 0 || height == 0 || gray_data.len() < (width * height) as usize {
            return (0.0, 0.0);
        }

        let w = width as i32;
        let h = height as i32;
        let cx = eye_x.round() as i32;
        let cy = eye_y.round() as i32;
        if cx < 0 || cx >= w || cy < 0 || cy >= h {
            return (0.0, 0.0);
        }

        let ed = eye_dist.max(20.0);
        let rx = (0.20 * ed).round().max(4.0) as i32;

        // 1. Eye aperture band: dy in [-0.07*ed .. +0.03*ed], dx in [-0.14*ed .. +0.14*ed].
        // Wider horizontal span allows the dark iris to sit near the inner/outer eye corner
        // during 15-25° head turns, while same-column vertical comparison against the lower
        // eyelid band [+0.08*ed .. +0.16*ed] cancels out horizontal nose-bridge shading.
        let irx = (0.14 * ed).round().max(3.0) as i32;
        let iry_top = (0.07 * ed).round().max(2.0) as i32;
        let iry_bot = (0.03 * ed).round().max(1.0) as i32;
        let l_top = (0.08 * ed).round().max(3.0) as i32;
        let l_bot = (0.16 * ed).round().max(6.0) as i32;

        let mut iris_vals = Vec::with_capacity(((2 * irx + 1) * (iry_top + iry_bot + 1)) as usize);
        let mut lower_vals = Vec::with_capacity(((2 * irx + 1) * (l_bot - l_top + 1)) as usize);
        let mut col_ldips = Vec::with_capacity((2 * irx + 1) as usize);

        for dx in -irx..=irx {
            let x = (cx + dx).clamp(0, w - 1) as usize;
            let mut c_iris = Vec::with_capacity((iry_top + iry_bot + 1) as usize);
            for dy in -iry_top..=iry_bot {
                let y = (cy + dy).clamp(0, h - 1) as usize;
                let p = gray_data[y * (width as usize) + x];
                c_iris.push(p);
                iris_vals.push(p);
            }
            let mut c_low = Vec::with_capacity((l_bot - l_top + 1) as usize);
            for dy in l_top..=l_bot {
                let y = (cy + dy).clamp(0, h - 1) as usize;
                let p = gray_data[y * (width as usize) + x];
                c_low.push(p);
                lower_vals.push(p);
            }
            c_iris.sort_unstable();
            c_low.sort_unstable();
            let ci_p25 = c_iris[c_iris.len() / 4] as f32;
            let cl_p75 = c_low[(3 * c_low.len()) / 4] as f32;
            col_ldips.push(cl_p75 - ci_p25);
        }

        if iris_vals.is_empty() || col_ldips.is_empty() {
            return (0.0, 0.0);
        }
        iris_vals.sort_unstable();
        lower_vals.sort_unstable();
        col_ldips.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let iris_p20 = iris_vals[iris_vals.len() / 5] as f32;
        let lower_ref = lower_vals[(3 * lower_vals.len()) / 4] as f32;
        let ldip = col_ldips[(7 * col_ldips.len()) / 10].max(0.0);

        // 2. Horizontal aperture statistics across [-rx .. +rx]
        let mut aperture_vals = Vec::with_capacity(((2 * rx + 1) * (iry_top + iry_bot + 1)) as usize);
        let mut row_std_sum = 0.0f32;
        let mut row_count = 0usize;
        let mut min_val = 255u8;
        let mut max_val = 0u8;
        let row_width = (2 * rx + 1) as f64;

        for dy in -iry_top..=iry_bot {
            let y = (cy + dy).clamp(0, h - 1) as usize;
            let row_off = y * (width as usize);
            let mut sum = 0.0f64;
            let mut sum_sq = 0.0f64;
            for dx in -rx..=rx {
                let x = (cx + dx).clamp(0, w - 1) as usize;
                let p = gray_data[row_off + x];
                aperture_vals.push(p);
                min_val = min_val.min(p);
                max_val = max_val.max(p);
                let pf = p as f64;
                sum += pf;
                sum_sq += pf * pf;
            }
            let mean = sum / row_width;
            let var = (sum_sq / row_width) - (mean * mean);
            row_std_sum += if var > 0.0 { var.sqrt() as f32 } else { 0.0 };
            row_count += 1;
        }
        for &p in &lower_vals {
            min_val = min_val.min(p);
            max_val = max_val.max(p);
        }

        aperture_vals.sort_unstable();
        let side_p80 = aperture_vals[(4 * aperture_vals.len()) / 5] as f32;
        let sdip = (side_p80 - iris_p20).max(0.0);

        let mean_row_std = if row_count > 0 {
            row_std_sum / (row_count as f32)
        } else {
            0.0
        };
        let raw_range = (max_val.saturating_sub(min_val)) as f32;

        // 3. Illumination normalization: boost dim/under-exposed frames (ref_skin < 130), never penalize bright frames
        let ref_skin = lower_ref.max(side_p80).max(iris_p20).max(35.0);
        let norm_scale = (130.0 / ref_skin).clamp(1.0, 2.2);

        let basin_dip = (ldip * sdip).sqrt();
        let eff_std = (mean_row_std * 0.45 * basin_dip).sqrt() * norm_scale;
        let eff_range = if basin_dip >= 12.0 {
            raw_range
        } else {
            basin_dip * 2.0 * norm_scale
        };

        (eff_std, eff_range)
    }

    pub fn cosine_similarity(feat1: &[f32], feat2: &[f32]) -> f32 {
        if feat1.len() != feat2.len() || feat1.is_empty() {
            return 0.0;
        }
        unsafe {
            face_engine_cosine_distance(feat1.as_ptr(), feat2.as_ptr(), feat1.len() as i32)
        }
    }
}

impl Drop for FaceEngine {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { face_engine_free(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_valid_face() -> Face {
        Face {
            bbox: (100.0, 100.0, 120.0, 140.0),
            score: 0.92,
            landmarks: [
                (130.0, 140.0), // right eye
                (190.0, 140.0), // left eye
                (160.0, 178.0), // nose (pitch ~0.54)
                (140.0, 210.0), // right mouth
                (180.0, 210.0), // left mouth
            ],
            embedding: vec![0.1; 128],
            eye_contrast: (18.0, 19.0),
            eye_range: (60.0, 65.0),
            eyes_visible: true,
        }
    }

    #[test]
    fn test_valid_human_face_check() {
        let face = sample_valid_face();
        assert!(face.is_valid_human_face());

        // Low confidence (e.g. pillow / false positive) rejected
        let mut weak_face = face.clone();
        weak_face.score = 0.55;
        assert!(!weak_face.is_valid_human_face());

        // Tiny box rejected
        let mut tiny_face = face.clone();
        tiny_face.bbox = (100.0, 100.0, 30.0, 30.0);
        assert!(!tiny_face.is_valid_human_face());

        // Inverted landmarks (mouth above eyes) rejected
        let mut inverted_face = face.clone();
        inverted_face.landmarks[3] = (140.0, 120.0);
        inverted_face.landmarks[4] = (180.0, 120.0);
        assert!(!inverted_face.is_valid_human_face());

        // Eyes occluded (covered by fingers, hands, or sleeping) rejected
        let mut occluded_face = face.clone();
        occluded_face.eyes_visible = false;
        assert!(!occluded_face.is_valid_human_face());
    }

    #[test]
    fn test_eye_metrics_computation() {
        // Flat image (e.g. fingers covering eyes)
        let flat_img = vec![120u8; 100 * 100];
        let (flat_std, flat_range) = FaceEngine::compute_eye_metrics(&flat_img, 100, 100, 50.0, 50.0, 40.0);
        assert_eq!(flat_std, 0.0);
        assert_eq!(flat_range, 0.0);

        // Pattern with high contrast (simulated pupil / sclera)
        let mut eye_img = vec![180u8; 100 * 100]; // bright sclera
        for y in 45..=55 {
            for x in 45..=55 {
                eye_img[y * 100 + x] = 30; // dark pupil/iris
            }
        }
        let (eye_std, eye_range) = FaceEngine::compute_eye_metrics(&eye_img, 100, 100, 50.0, 50.0, 40.0);
        assert!(eye_std > 15.0);
        assert_eq!(eye_range, 150.0);

        // Simulated 20° head turn where iris shifts towards the corner of the eye landmark (±5px)
        let (turn_l_std, _) = FaceEngine::compute_eye_metrics(&eye_img, 100, 100, 45.0, 50.0, 40.0);
        let (turn_r_std, _) = FaceEngine::compute_eye_metrics(&eye_img, 100, 100, 55.0, 50.0, 40.0);
        assert!(turn_l_std > 15.0);
        assert!(turn_r_std > 15.0);

        // Closed eyelid: strong vertical shading (bright upper lid -> darker lash line) but no central dark iris basin
        let mut closed_img = vec![150u8; 100 * 100];
        for y in 42..=58 {
            let row_val = (185 - (y as i32 - 42) * 5).clamp(100, 200) as u8;
            for x in 35..=65 {
                closed_img[y * 100 + x] = row_val;
            }
        }
        let (closed_std, _) = FaceEngine::compute_eye_metrics(&closed_img, 100, 100, 50.0, 50.0, 40.0);
        assert!(closed_std < 5.0, "Closed eyelid vertical gradient should have low openness score, got {}", closed_std);
    }

    #[test]
    fn test_pose_matching() {
        let center_face = sample_valid_face();
        assert!(center_face.matches_target_pose("center").0);

        // Turn head left: nose moves towards left eye (higher X in unmirrored image)
        let mut left_face = sample_valid_face();
        left_face.landmarks[2] = (175.0, 175.0); // nose close to left eye (190.0) -> yaw = 45/60 = 0.75
        assert!(left_face.matches_target_pose("look_left").0);
        assert!(!left_face.matches_target_pose("center").0);

        // Turn head right: nose moves towards right eye (lower X in unmirrored image)
        let mut right_face = sample_valid_face();
        right_face.landmarks[2] = (145.0, 175.0); // nose close to right eye (130.0) -> yaw = 15/60 = 0.25
        assert!(right_face.matches_target_pose("look_right").0);
        assert!(!right_face.matches_target_pose("center").0);

        // Tilt up: nose moves closer to eye line
        let mut up_face = sample_valid_face();
        up_face.landmarks[2] = (160.0, 155.0); // eye_y=140, mouth_y=210, dist=70. nose_y=155 -> pitch=15/70=0.21
        assert!(up_face.matches_target_pose("look_up").0);
        assert!(!up_face.matches_target_pose("center").0);

        // Tilt down: nose moves closer to mouth line
        let mut down_face = sample_valid_face();
        down_face.landmarks[2] = (160.0, 195.0); // nose_y=195 -> pitch=55/70=0.78
        assert!(down_face.matches_target_pose("look_down").0);
        assert!(!down_face.matches_target_pose("center").0);
    }
}

