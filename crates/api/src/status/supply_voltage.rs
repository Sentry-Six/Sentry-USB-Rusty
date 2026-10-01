//! Capability-based 5 V supply measurements from the Raspberry Pi PMIC.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const MAX_SAMPLE_AGE: Duration = Duration::from_secs(5);
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct Sample {
    voltage: Option<f64>,
    taken_at: Instant,
}

impl Sample {
    fn value_at(self, now: Instant) -> Option<f64> {
        self.voltage.filter(|_| now.duration_since(self.taken_at) < MAX_SAMPLE_AGE)
    }

    fn needs_refresh(self, now: Instant) -> bool {
        let interval = if self.voltage.is_some() { REFRESH_INTERVAL } else { UNAVAILABLE_RETRY };
        now.duration_since(self.taken_at) >= interval
    }
}

#[derive(Default)]
struct Cache {
    sample: Mutex<Option<Sample>>,
    refresh: Arc<tokio::sync::Mutex<()>>,
}

impl Cache {
    /// Return promptly, even on the first request. Concurrent clients share one
    /// background measurement; unavailable sensors are retried less frequently.
    fn get_or_refresh<F, Fut>(self: &Arc<Self>, read: F) -> Option<f64>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Option<f64>> + Send,
    {
        let previous = *self.sample.lock().unwrap();
        let now = Instant::now();
        let value = previous.and_then(|sample| sample.value_at(now));
        if previous.is_some_and(|sample| !sample.needs_refresh(now)) {
            return value;
        }
        if let Ok(guard) = Arc::clone(&self.refresh).try_lock_owned() {
            // A refresh may have completed between the cache read and lock.
            let previous = *self.sample.lock().unwrap();
            let now = Instant::now();
            if previous.is_some_and(|sample| !sample.needs_refresh(now)) {
                return previous.and_then(|sample| sample.value_at(now));
            }
            let cache = Arc::clone(self);
            tokio::spawn(async move {
                let _guard = guard;
                let voltage = read().await;
                // A failed measurement replaces, rather than preserves, the old reading.
                *cache.sample.lock().unwrap() = Some(Sample { voltage, taken_at: Instant::now() });
            });
        }
        value
    }
}

pub(super) fn get() -> Option<f64> {
    static CACHE: OnceLock<Arc<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Cache::default())).get_or_refresh(read)
}

async fn read() -> Option<f64> {
    // `measure_volts` reports internal rails, not the 5 V input. PMIC ADC support
    // is detected from the actual result instead of guessing from a model name.
    // run_with_timeout uses kill_on_drop, so a stuck firmware call cannot accumulate.
    let output = sentryusb_shell::run_with_timeout(
        COMMAND_TIMEOUT,
        "vcgencmd",
        &["pmic_read_adc", "EXT5V_V"],
    ).await.ok()?;
    parse(&output)
}

fn parse(output: &str) -> Option<f64> {
    // Example: `EXT5V_V volt(24)=5.10540000V`. Some firmware returns all rails
    // even when one is requested, so select the exact rail before parsing.
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("EXT5V_V") {
            continue;
        }
        let reading = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        let (channel, value) = reading.split_once('=')?;
        let channel = channel.strip_prefix("volt(")?.strip_suffix(')')?;
        if channel.is_empty() || !channel.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let voltage = value.strip_suffix('V')?.parse::<f64>().ok()?;
        return (voltage.is_finite() && voltage > 0.0).then_some(voltage);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parses_input_voltage_without_confusing_it_with_internal_rails() {
        assert_eq!(parse("   EXT5V_V volt(24)=5.10540000V\r\n"), Some(5.1054));
        assert_eq!(parse("VDD_CORE_V volt(15)=0.90927870V\nHDMI_V volt(23)=5.11746000V\nEXT5V_V volt(24)=4.72000000V\nBATT_V volt(25)=0.00427350V\n"), Some(4.72));
        assert_eq!(parse("volt=0.9081V"), None);
        assert_eq!(parse("VDD_CORE_V volt(15)=0.90927870V"), None);
        assert_eq!(parse("EXT5V_V_EXTRA volt(24)=5.1V"), None);
    }

    #[test]
    fn rejects_unavailable_malformed_and_nonphysical_values() {
        for output in [
            "", "error=1 error_msg=\"Command not registered\"", "EXT5V_V",
            "EXT5V_V volt(24)=5.1", "EXT5V_V volt(24)=5.1A", "EXT5V_V current(24)=5.1V",
            "EXT5V_V volt()=5.1V", "EXT5V_V volt(bad)=5.1V", "EXT5V_V volt(24)=5.1V extra",
            "EXT5V_V volt(24)=NaNV", "EXT5V_V volt(24)=infV", "EXT5V_V volt(24)=-infV",
            "EXT5V_V volt(24)=0V", "EXT5V_V volt(24)=-1V", "EXT5V_V volt(24)=1e999V",
        ] {
            assert_eq!(parse(output), None, "accepted invalid PMIC output: {output}");
        }
    }

    #[test]
    fn measurements_expire_and_unavailable_sensors_back_off() {
        let now = Instant::now();
        let sample = Sample { voltage: Some(5.1), taken_at: now };
        assert!(!sample.needs_refresh(now + Duration::from_secs(1)));
        assert!(sample.needs_refresh(now + REFRESH_INTERVAL));
        assert_eq!(sample.value_at(now + REFRESH_INTERVAL), Some(5.1));
        assert_eq!(sample.value_at(now + MAX_SAMPLE_AGE), None);
        let unavailable = Sample { voltage: None, taken_at: now };
        assert!(!unavailable.needs_refresh(now + MAX_SAMPLE_AGE));
        assert!(unavailable.needs_refresh(now + UNAVAILABLE_RETRY));
        assert_eq!(unavailable.value_at(now), None);
    }

    #[tokio::test]
    async fn concurrent_pollers_share_one_measurement_without_waiting() {
        let cache = Arc::new(Cache::default());
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let (finish, pending) = tokio::sync::oneshot::channel();
        assert_eq!(cache.get_or_refresh(|| async move {
            counter.fetch_add(1, Ordering::Relaxed);
            pending.await.unwrap()
        }), None);
        for _ in 0..20 {
            assert_eq!(cache.get_or_refresh(|| async { panic!("duplicate PMIC command") }), None);
        }
        finish.send(Some(5.12)).unwrap();
        // Waiting on the refresh lock gives deterministic completion without sleeps.
        drop(cache.refresh.lock().await);
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        assert_eq!(cache.get_or_refresh(|| async { panic!("fresh cache was ignored") }), Some(5.12));
    }

    #[tokio::test]
    async fn failed_refresh_invalidates_previous_reading_and_backs_off() {
        let cache = Arc::new(Cache::default());
        *cache.sample.lock().unwrap() = Some(Sample {
            voltage: Some(5.1), taken_at: Instant::now() - REFRESH_INTERVAL,
        });
        assert_eq!(cache.get_or_refresh(|| async { None }), Some(5.1));
        drop(cache.refresh.lock().await);
        assert_eq!(cache.get_or_refresh(|| async { panic!("failure retry was not limited") }), None);
        // An expired unavailable result can recover without restarting the API.
        cache.sample.lock().unwrap().as_mut().unwrap().taken_at = Instant::now() - UNAVAILABLE_RETRY;
        assert_eq!(cache.get_or_refresh(|| async { Some(4.75) }), None);
        drop(cache.refresh.lock().await);
        assert_eq!(cache.get_or_refresh(|| async { panic!("fresh cache was ignored") }), Some(4.75));
    }

    #[tokio::test]
    async fn stale_success_is_not_returned_while_a_refresh_is_pending() {
        let cache = Arc::new(Cache::default());
        *cache.sample.lock().unwrap() = Some(Sample {
            voltage: Some(5.1), taken_at: Instant::now() - MAX_SAMPLE_AGE,
        });
        let (finish, pending) = tokio::sync::oneshot::channel();
        assert_eq!(cache.get_or_refresh(|| async move { pending.await.unwrap() }), None);
        assert_eq!(cache.get_or_refresh(|| async { panic!("duplicate PMIC command") }), None);
        finish.send(Some(4.8)).unwrap();
        drop(cache.refresh.lock().await);
        assert_eq!(cache.get_or_refresh(|| async { panic!("fresh cache was ignored") }), Some(4.8));
    }
}
