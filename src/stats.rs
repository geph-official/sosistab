use std::time::SystemTime;

use dashmap::DashMap;
use probability::distribution::Inverse;

/// Min-queue
#[derive(Debug, Clone, Default)]
pub struct MinQueue<T: Ord> {
    left: Vec<(usize, usize)>,
    right: Vec<(usize, usize)>,
    items: slab::Slab<T>,
}

impl<T: Ord> MinQueue<T> {
    /// Creates something empty.
    pub fn new() -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
            items: slab::Slab::new(),
        }
    }

    /// Gets the length.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Pushes something to the back of the queue.
    pub fn push_back(&mut self, elem: T) {
        let elem = self.items.insert(elem);
        self.left.push((
            elem,
            self.left
                .last()
                .copied()
                .map(|i| self.min_idx_of(i.1, elem))
                .unwrap_or(elem),
        ));
    }

    /// Pops from the beginning of the queue.
    pub fn pop_front(&mut self) -> Option<T> {
        if self.right.is_empty() {
            while let Some((elem, _)) = self.left.pop() {
                self.right.push((
                    elem,
                    self.right
                        .last()
                        .copied()
                        .map(|i| self.min_idx_of(i.1, elem))
                        .unwrap_or(elem),
                ));
            }
        }
        Some(self.items.remove(self.right.pop()?.0))
    }

    /// Peeks the beginning of the queue.
    pub fn peek_front(&mut self) -> Option<&T> {
        self.items.get(if self.right.is_empty() {
            self.left.first().copied()?.0
        } else {
            self.right.last().copied()?.0
        })
    }

    /// Get current minimum.
    pub fn min(&self) -> Option<&T> {
        self.items.get(self.min_idx()?)
    }

    /// Get current minimum index.
    fn min_idx(&self) -> Option<usize> {
        Some(if self.right.is_empty() {
            self.left.last().copied()?.1
        } else if self.left.is_empty() {
            self.right.last().copied()?.1
        } else {
            self.min_idx_of(self.left.last().copied()?.1, self.right.last().copied()?.1)
        })
    }

    /// Get smaller of two
    fn min_idx_of(&self, x: usize, y: usize) -> usize {
        if self.items[x] < self.items[y] {
            x
        } else {
            y
        }
    }
}

/// Exponential moving average and standard deviation calculator
#[derive(Debug, Clone)]
pub struct EmaCalculator {
    mean_accum: f64,
    variance_accum: f64,
    set: bool,
    alpha: f64,
}

impl EmaCalculator {
    /// Creates a new calculator with the given initial estimate and smoothing factor (which should be close to 0)
    pub fn new(initial_mean: f64, alpha: f64) -> Self {
        Self {
            mean_accum: initial_mean,
            variance_accum: initial_mean.powi(2),
            alpha,
            set: true,
        }
    }

    /// Creates a new calculator with nothing set.
    pub fn new_unset(alpha: f64) -> Self {
        Self {
            mean_accum: 0.0,
            variance_accum: 0.001,
            alpha,
            set: false,
        }
    }

    /// Updates the calculator with a given data point
    pub fn update(&mut self, point: f64) {
        if !self.set {
            self.mean_accum = point;
            self.variance_accum = 0.0;
            self.set = true
        }
        // https://stats.stackexchange.com/questions/111851/standard-deviation-of-an-exponentially-weighted-mean
        self.variance_accum = (1.0 - self.alpha)
            * (self.variance_accum + self.alpha * (point - self.mean_accum).powi(2));
        self.mean_accum = self.mean_accum * (1.0 - self.alpha) + self.alpha * point;
    }

    /// Gets a very rough approximation (normal approximation) of the given percentile
    pub fn inverse_cdf(&self, frac: f64) -> f64 {
        let stddev = self.variance_accum.sqrt();
        if stddev > 0.0 {
            let dist = probability::distribution::Gaussian::new(
                self.mean_accum,
                self.variance_accum.sqrt(),
            );
            dist.inverse(frac)
        } else {
            self.mean_accum
        }
    }

    /// Gets the current mean
    pub fn mean(&self) -> f64 {
        self.mean_accum
    }
}

/// A generic statistics gatherer, logically a string-keyed map of f64-valued time series. It has a fairly cheap Clone implementation, allowing easy "snapshots" of the stats at a given point in time. The Default implementation creates a no-op that does nothing.
#[derive(Debug, Clone, Default)]
pub struct StatsGatherer {
    mapping: Option<DashMap<String, TimeSeries>>,
}

impl StatsGatherer {
    /// Creates a usable statistics gatherer. Unlike the Default implementation, this one actually does something.
    pub fn new_active() -> Self {
        Self {
            mapping: Some(Default::default()),
        }
    }

    /// Updates a statistical item.
    pub fn update(&self, stat: &str, val: f32) {
        if let Some(mapping) = &self.mapping {
            let mut ts = mapping
                .entry(stat.to_string())
                .or_insert_with(|| TimeSeries::new(10000));
            ts.push(val)
        }
    }

    /// Increments a statistical item.
    pub fn increment(&self, stat: &str, delta: f32) {
        if let Some(mapping) = &self.mapping {
            let mut ts = mapping
                .entry(stat.to_string())
                .or_insert_with(|| TimeSeries::new(10000));
            ts.increment(delta)
        }
    }

    /// Obtains the last value of a statistical item.
    pub fn get_last(&self, stat: &str) -> Option<f32> {
        let series = self.mapping.as_ref()?.get(stat)?;
        series.items.get_max().map(|v| v.1)
    }

    /// Obtains the whole TimeSeries, taking ownership of a snapshot.
    pub fn get_timeseries(&self, stat: &str) -> Option<TimeSeries> {
        Some(self.mapping.as_ref()?.get(stat)?.clone())
    }

    /// Iterates through all the TimeSeries in this stats gatherer.
    pub fn iter(&self) -> impl Iterator<Item = (String, TimeSeries)> {
        self.mapping.clone().unwrap_or_default().into_iter()
    }
}

/// A time-series that is just a time-indexed vector of f32s that automatically decimates and compacts old data.
#[derive(Debug, Clone, Default)]
pub struct TimeSeries {
    max_length: usize,
    items: im::OrdMap<SystemTime, f32>,
}

impl TimeSeries {
    /// Pushes a new item into the time series.
    pub fn push(&mut self, item: f32) {
        if self.same_as_last() {
            return;
        }
        self.items.insert(SystemTime::now(), item);
        self.may_decimate()
    }

    fn may_decimate(&mut self) {
        if self.items.len() >= self.max_length {
            // decimate the whole vector
            let half_map: im::OrdMap<_, _> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, v)| if i % 2 != 0 { Some((*v.0, *v.1)) } else { None })
                .collect();
            self.items = half_map;
        }
    }

    fn same_as_last(&self) -> bool {
        let last = self.items.get_max();
        if let Some(last) = last {
            let last_time = last.0;
            let dur = last_time.elapsed();
            if let Ok(dur) = dur {
                if dur.as_millis() < 5 {
                    return true;
                }
            }
        }
        false
    }

    /// Pushes a new item into the time series.
    pub fn increment(&mut self, delta: f32) {
        if self.same_as_last() {
            let (last_key, last_val) = self.items.get_max().unwrap();
            let last_key = *last_key;
            let new_last = *last_val + delta;
            self.items.insert(last_key, new_last);
            return;
        }
        let last_val = self.items.get_max().map(|v| v.1).unwrap_or_default();
        self.items.insert(SystemTime::now(), delta + last_val);
        self.may_decimate()
    }

    /// Create a new time series with a given maximum length.
    pub fn new(max_length: usize) -> Self {
        Self {
            max_length,
            items: im::OrdMap::new(),
        }
    }

    /// Get an iterator over the elements
    pub fn iter(&self) -> impl Iterator<Item = (&SystemTime, &f32)> {
        self.items.iter()
    }

    /// Restricts the time series to points after a certain time.
    pub fn after(&self, time: SystemTime) -> Self {
        let (_, after) = self.items.split(&time);
        Self {
            items: after,
            max_length: self.max_length,
        }
    }

    /// Get the value at a certain time.
    pub fn get(&self, time: SystemTime) -> f32 {
        self.items
            .get_prev(&time)
            .map(|v| *v.1)
            .unwrap_or_default()
            .max(self.items.get_next(&time).map(|v| *v.1).unwrap_or_default())
    }

    /// Get the earliest time.
    pub fn earliest(&self) -> Option<(SystemTime, f32)> {
        self.items.get_min().cloned()
    }
}
