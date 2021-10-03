use crate::types::{Camera, CameraProjection};
use glam::{Mat3A, Mat4, Vec3, Vec3A};

/// Manages the camera's location and projection settings.
#[derive(Debug, Clone)]
pub struct CameraManager {
    orig_view: Mat4,
    view: Mat4,
    proj: Mat4,
    data: Camera,
    aspect_ratio: f32,
}
impl CameraManager {
    /// Builds a new camera, using the given aspect ratio. If no aspect ratio is given
    /// it is assumed that no aspect ratio scaling should be done.
    pub fn new(data: Camera, aspect_ratio: Option<f32>) -> Self {
        profiling::scope!("CameraManager::new");

        let aspect_ratio = aspect_ratio.unwrap_or(1.0);
        let proj = compute_projection_matrix(data, aspect_ratio);
        let view = compute_view_matrix(data);
        let orig_view = compute_origin_matrix(data);

        Self {
            orig_view,
            view,
            proj,
            data,
            aspect_ratio,
        }
    }

    /// Sets the camera data, rebuilding the using the given aspect ratio. If no aspect ratio is given
    /// it is assumed that no aspect ratio scaling should be done.
    pub fn set_data(&mut self, data: Camera) {
        self.set_aspect_data(data, self.aspect_ratio)
    }

    pub fn set_aspect_ratio(&mut self, aspect_ratio: Option<f32>) {
        self.set_aspect_data(self.data, aspect_ratio.unwrap_or(1.0));
    }

    pub fn set_aspect_data(&mut self, data: Camera, aspect_ratio: f32) {
        self.proj = compute_projection_matrix(data, self.aspect_ratio);
        self.view = compute_view_matrix(data);
        self.orig_view = compute_origin_matrix(data);
        self.data = data;
        self.aspect_ratio = aspect_ratio;
    }

    pub fn get_data(&self) -> Camera {
        self.data
    }

    pub fn view(&self) -> Mat4 {
        self.view
    }

    pub fn view_proj(&self) -> Mat4 {
        self.proj * self.view
    }

    pub fn origin_view_proj(&self) -> Mat4 {
        self.proj * self.orig_view
    }

    pub fn proj(&self) -> Mat4 {
        self.proj
    }
}

fn compute_look_offset(data: Camera) -> Vec3A {
    match data.projection {
        CameraProjection::Projection { pitch, yaw, .. } => {
            let starting = Vec3A::Z;
            Mat3A::from_euler(glam::EulerRot::YXZ, yaw, pitch, 0.0) * starting
        }
        CameraProjection::Orthographic { direction, .. } => direction,
    }
}

fn compute_view_matrix(data: Camera) -> Mat4 {
    let look_offset = compute_look_offset(data);

    Mat4::look_at_lh(
        Vec3::from(data.location),
        Vec3::from(data.location + look_offset),
        Vec3::Y,
    )
}

fn compute_projection_matrix(data: Camera, aspect_ratio: f32) -> Mat4 {
    match data.projection {
        CameraProjection::Orthographic { size, .. } => {
            let half = size * 0.5;
            Mat4::orthographic_lh(-half.x, half.x, -half.y, half.y, -half.z, half.z)
        }
        CameraProjection::Projection { vfov, near, .. } => {
            Mat4::perspective_infinite_reverse_lh(vfov.to_radians(), aspect_ratio, near)
        }
    }
}

// This is horribly inefficient but is called like once a frame.
fn compute_origin_matrix(data: Camera) -> Mat4 {
    let look_offset = compute_look_offset(data);

    Mat4::look_at_lh(Vec3::ZERO, Vec3::from(look_offset), Vec3::Y)
}
