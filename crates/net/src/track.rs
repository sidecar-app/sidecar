use std::sync::Arc;

use webrtc::api::media_engine::MIME_TYPE_VP9;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

/// A single viewer's video track.
///
/// Each viewer owns its own `ViewerTrack` so its RTP packetizer, payload type,
/// and header extensions match that viewer's own SDP negotiation. Sharing one
/// track across heterogeneous clients (e.g. Firefox and Chrome negotiating AV1
/// differently) corrupts the stream, so tracks are intentionally not shared.
///
/// Wraps [`TrackLocalStaticSample`]; frames are written to it by the session's
/// per-viewer writer task.
pub struct ViewerTrack {
    track: Arc<TrackLocalStaticSample>,
}

impl ViewerTrack {
    /// Create a new shared video track with the given codec MIME type
    /// (e.g. `"video/VP9"`, `"video/AV1"`).
    pub fn new(mime_type: &str) -> Self {
        let track = TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: mime_type.to_owned(),
                ..Default::default()
            },
            "video".to_owned(),
            "screenshare".to_owned(),
        );
        Self {
            track: Arc::new(track),
        }
    }

    /// Get a clone of the inner track `Arc` for adding to peer connections.
    pub fn track(&self) -> Arc<TrackLocalStaticSample> {
        Arc::clone(&self.track)
    }

    /// Get the track as a `dyn TrackLocal` for adding to peer connections via `add_track`.
    pub fn as_track_local(&self) -> Arc<dyn TrackLocal + Send + Sync> {
        Arc::clone(&self.track) as Arc<dyn TrackLocal + Send + Sync>
    }
}

impl Default for ViewerTrack {
    fn default() -> Self {
        Self::new(MIME_TYPE_VP9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_video_track_creates_with_vp9() {
        let track = ViewerTrack::new("video/VP9");
        let _local = track.as_track_local();
    }

    #[test]
    fn shared_video_track_creates_with_av1() {
        let track = ViewerTrack::new("video/AV1");
        let _local = track.as_track_local();
    }
}
