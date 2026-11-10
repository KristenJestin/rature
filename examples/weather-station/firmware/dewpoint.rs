/// The dew point in °C, from the temperature in °C and the relative humidity in %
/// (Magnus approximation).
pub fn dew_point(t: f64, rh: f64) -> f64 {
    let (a, b) = (17.62, 243.12);
    let gamma = (rh / 100.0).ln() + a * t / (b + t);
    b * gamma / (a - gamma)
}

/// The mean of the last readings, the newest last; none yet gives `None`.
pub fn rolling_average(readings: &[f64]) -> Option<f64> {
    (!readings.is_empty()).then(|| readings.iter().sum::<f64>() / readings.len() as f64)
}
