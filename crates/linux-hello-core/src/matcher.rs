use crate::engine::{Face, FaceEngine};
use crate::storage::UserProfile;

pub const DEFAULT_COSINE_THRESHOLD: f32 = 0.50;

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub matched: bool,
    pub best_score: f32,
    pub matched_label: Option<String>,
}

pub fn verify_face(face: &Face, profile: &UserProfile, threshold: f32) -> MatchResult {
    let mut best_score = -1.0f32;
    let mut best_label = None;

    for (i, enrolled_embedding) in profile.enrolled_embeddings.iter().enumerate() {
        let score = FaceEngine::cosine_similarity(&face.embedding, enrolled_embedding);
        if score > best_score {
            best_score = score;
            best_label = profile.labels.get(i).cloned();
        }
    }

    MatchResult {
        matched: best_score >= threshold,
        best_score,
        matched_label: best_label,
    }
}
