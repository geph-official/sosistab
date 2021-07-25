use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use std::{
    borrow::Borrow,
    cell::RefCell,
    cmp::Ordering,
    ops::{Bound, Deref, DerefMut, RangeBounds},
    sync::Arc,
};

thread_local! {
    static BUFF_POOL: RefCell<Vec<Vec<u8>>> = Default::default()
}

/// Represents a *mutable* buffer optimized for packet-sized payloads.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct BuffMut {
    inner: Vec<u8>,
}

impl Deref for BuffMut {
    type Target = Vec<u8>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for BuffMut {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Drop for BuffMut {
    #[inline]
    fn drop(&mut self) {
        if self.capacity() > 4096 {
            tracing::debug!("freeing oversize {}", self.capacity());
            return;
        }
        BUFF_POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < 1000 {
                pool.push(std::mem::take(&mut self.inner));
            }
        })
    }
}

impl Default for BuffMut {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl BuffMut {
    /// Creates a new BuffMut
    #[inline]
    pub fn new() -> Self {
        let new_vec = BUFF_POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            let mut v = pool.pop().unwrap_or_else(|| {
                tracing::warn!("pool empty, allocating from malloc");
                Vec::with_capacity(2048)
            });
            v.truncate(0);
            v
        });
        Self { inner: new_vec }
    }

    /// Freezes the BuffMut into a Buff.
    #[inline]
    pub fn freeze(self) -> Buff {
        Buff {
            frozen: Arc::new(self),
            bounds: (Bound::Unbounded, Bound::Unbounded),
        }
    }

    /// Copies from a slice.
    #[inline]
    pub fn copy_from_slice(other: &[u8]) -> Self {
        let mut m = Self::new();
        m.extend_from_slice(other);
        m
    }
}

/// Represents an *immutable* buffer.
#[derive(Clone, Debug, Deserialize)]
#[serde(from = "BuffMut")]
pub struct Buff {
    frozen: Arc<BuffMut>,
    bounds: (Bound<usize>, Bound<usize>),
}

impl PartialEq<Buff> for Buff {
    #[inline]
    fn eq(&self, other: &Buff) -> bool {
        self.deref() == other.deref()
    }
}

impl Eq for Buff {}

impl PartialOrd<Buff> for Buff {
    #[inline]
    fn partial_cmp(&self, other: &Buff) -> Option<Ordering> {
        self.deref().partial_cmp(other.deref())
    }
}

impl Ord for Buff {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.deref().cmp(other.deref())
    }
}

impl Default for Buff {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Buff {
    /// Creates a new, empty Buff.
    #[inline]
    pub fn new() -> Self {
        Self::copy_from_slice(&[])
    }
    /// "Slices" the buff, making another buff. Takes ownership to prevent unnecessary cloning.
    #[inline]
    pub fn slice(self, bounds: impl RangeBounds<usize>) -> Self {
        // make sure not OOB
        let loo: &[u8] = self.as_ref();
        let start_bound = match bounds.start_bound() {
            Bound::Excluded(bound) => Bound::Excluded(*bound),
            Bound::Included(bound) => Bound::Included(*bound),
            Bound::Unbounded => Bound::Unbounded,
        };
        let end_bound = match bounds.end_bound() {
            Bound::Excluded(bound) => Bound::Excluded(*bound),
            Bound::Included(bound) => Bound::Included(*bound),
            Bound::Unbounded => Bound::Unbounded,
        };
        // intentionally trigger panic if OOB
        let _ = &loo[(start_bound, end_bound)];
        Self {
            frozen: self.frozen,
            bounds: (start_bound, end_bound),
        }
    }

    /// Creates a new buff by copying from a slice
    #[inline]
    pub fn copy_from_slice(other: &[u8]) -> Self {
        let mut inner = BuffMut::new();
        inner.extend_from_slice(other);
        inner.freeze()
    }
}

impl Deref for Buff {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl From<BuffMut> for Buff {
    #[inline]
    fn from(m: BuffMut) -> Self {
        m.freeze()
    }
}

impl From<&[u8]> for Buff {
    #[inline]
    fn from(m: &[u8]) -> Self {
        Self::copy_from_slice(m)
    }
}

impl Serialize for Buff {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let buf: &[u8] = self.as_ref();
        buf.serialize(serializer)
    }
}

impl AsRef<[u8]> for Buff {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.frozen.as_slice()[self.bounds]
    }
}

impl Borrow<[u8]> for Buff {
    #[inline]
    fn borrow(&self) -> &[u8] {
        &self.frozen.as_slice()[self.bounds]
    }
}

impl<'de> Deserialize<'de> for BuffMut {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_byte_buf(BuffMutVisitor {})
    }
}

struct BuffMutVisitor;

impl<'de> Visitor<'de> for BuffMutVisitor {
    type Value = BuffMut;

    #[inline]
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a byte array")
    }

    #[inline]
    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let mut bm = BuffMut::new();
        bm.extend_from_slice(v);
        Ok(bm)
    }

    #[inline]
    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(BuffMut { inner: v })
    }
}
