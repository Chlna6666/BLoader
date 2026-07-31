pub fn animate_scalar(current: f32, target: f32, speed: f32, delta_seconds: f32) -> f32 {
    let alpha = 1.0 - (-speed * delta_seconds.max(0.0)).exp();
    current + (target - current) * alpha.clamp(0.0, 1.0)
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let x = 1.0 - t.clamp(0.0, 1.0);
    1.0 - x * x * x
}
