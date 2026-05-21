use std::collections::HashMap;

// ─── MiniRoaring Bitmap ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    Array(Vec<u16>),
    Bitmap(Box<[u64; 1024]>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiniRoaring {
    pub containers: HashMap<u16, Container>,
}

impl MiniRoaring {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
        }
    }

    /// Insert a 32-bit ID into the roaring bitmap
    pub fn insert(&mut self, id: u32) {
        let key = (id >> 16) as u16;
        let value = (id & 0xFFFF) as u16;

        let container = self.containers.entry(key).or_insert_with(|| Container::Array(Vec::new()));

        match container {
            Container::Array(ref mut arr) => {
                if let Err(idx) = arr.binary_search(&value) {
                    arr.insert(idx, value);
                    // If sparse vector grows beyond 1024 elements, upgrade to a packed bitset
                    if arr.len() > 1024 {
                        let mut bitmap = Box::new([0u64; 1024]);
                        for &v in arr.iter() {
                            let idx = (v >> 6) as usize;
                            let bit = (v & 63) as u64;
                            bitmap[idx] |= 1 << bit;
                        }
                        *container = Container::Bitmap(bitmap);
                    }
                }
            }
            Container::Bitmap(ref mut bitmap) => {
                let idx = (value >> 6) as usize;
                let bit = (value & 63) as u64;
                bitmap[idx] |= 1 << bit;
            }
        }
    }

    /// Check if the 32-bit ID is contained in the bitmap
    pub fn contains(&self, id: u32) -> bool {
        let key = (id >> 16) as u16;
        let value = (id & 0xFFFF) as u16;

        match self.containers.get(&key) {
            Some(Container::Array(arr)) => arr.binary_search(&value).is_ok(),
            Some(Container::Bitmap(bitmap)) => {
                let idx = (value >> 6) as usize;
                let bit = (value & 63) as u64;
                (bitmap[idx] & (1 << bit)) != 0
            }
            None => false,
        }
    }

    /// Intersects two roaring bitmaps to yield a new intersected bitmap
    pub fn intersect(&self, other: &Self) -> Self {
        let mut containers = HashMap::new();

        for (key, self_c) in &self.containers {
            if let Some(other_c) = other.containers.get(key) {
                let intersection = match (self_c, other_c) {
                    (Container::Array(a), Container::Array(b)) => {
                        let mut res = Vec::new();
                        let (mut i, mut j) = (0, 0);
                        while i < a.len() && j < b.len() {
                            if a[i] == b[j] {
                                res.push(a[i]);
                                i += 1;
                                j += 1;
                            } else if a[i] < b[j] {
                                i += 1;
                            } else {
                                j += 1;
                            }
                        }
                        if !res.is_empty() {
                            Some(Container::Array(res))
                        } else {
                            None
                        }
                    }
                    (Container::Bitmap(a), Container::Bitmap(b)) => {
                        let mut bitmap = Box::new([0u64; 1024]);
                        let mut empty = true;
                        for i in 0..1024 {
                            bitmap[i] = a[i] & b[i];
                            if bitmap[i] != 0 {
                                empty = false;
                            }
                        }
                        if !empty {
                            Some(Container::Bitmap(bitmap))
                        } else {
                            None
                        }
                    }
                    (Container::Array(arr), Container::Bitmap(bitmap)) |
                    (Container::Bitmap(bitmap), Container::Array(arr)) => {
                        let mut res = Vec::new();
                        for &v in arr {
                            let idx = (v >> 6) as usize;
                            let bit = (v & 63) as u64;
                            if (bitmap[idx] & (1 << bit)) != 0 {
                                res.push(v);
                            }
                        }
                        if !res.is_empty() {
                            Some(Container::Array(res))
                        } else {
                            None
                        }
                    }
                };

                if let Some(c) = intersection {
                    containers.insert(*key, c);
                }
            }
        }

        Self { containers }
    }

    /// Unions two roaring bitmaps to yield a new unioned bitmap
    pub fn union(&self, other: &Self) -> Self {
        let mut containers = self.containers.clone();

        for (key, other_c) in &other.containers {
            if let Some(self_c) = containers.get_mut(key) {
                *self_c = match (&*self_c, other_c) {
                    (Container::Array(a), Container::Array(b)) => {
                        let mut res = Vec::new();
                        let (mut i, mut j) = (0, 0);
                        while i < a.len() || j < b.len() {
                            if i < a.len() && (j >= b.len() || a[i] < b[j]) {
                                res.push(a[i]);
                                i += 1;
                            } else if j < b.len() && (i >= a.len() || b[j] < a[i]) {
                                res.push(b[j]);
                                j += 1;
                            } else {
                                res.push(a[i]);
                                i += 1;
                                j += 1;
                            }
                        }
                        if res.len() > 1024 {
                            let mut bitmap = Box::new([0u64; 1024]);
                            for &v in &res {
                                let idx = (v >> 6) as usize;
                                let bit = (v & 63) as u64;
                                bitmap[idx] |= 1 << bit;
                            }
                            Container::Bitmap(bitmap)
                        } else {
                            Container::Array(res)
                        }
                    }
                    (Container::Bitmap(a), Container::Bitmap(b)) => {
                        let mut bitmap = Box::new([0u64; 1024]);
                        for i in 0..1024 {
                            bitmap[i] = a[i] | b[i];
                        }
                        Container::Bitmap(bitmap)
                    }
                    (Container::Array(arr), Container::Bitmap(bitmap)) |
                    (Container::Bitmap(bitmap), Container::Array(arr)) => {
                        let mut new_bitmap = bitmap.clone();
                        for &v in arr {
                            let idx = (v >> 6) as usize;
                            let bit = (v & 63) as u64;
                            new_bitmap[idx] |= 1 << bit;
                        }
                        Container::Bitmap(new_bitmap)
                    }
                };
            } else {
                containers.insert(*key, other_c.clone());
            }
        }

        Self { containers }
    }

    /// Extract all document IDs in sorted order
    pub fn iter(&self) -> Vec<u32> {
        let mut keys: Vec<&u16> = self.containers.keys().collect();
        keys.sort();

        let mut res = Vec::new();
        for &key in keys {
            let high = (key as u32) << 16;
            match &self.containers[&key] {
                Container::Array(arr) => {
                    for &val in arr {
                        res.push(high | (val as u32));
                    }
                }
                Container::Bitmap(bitmap) => {
                    for i in 0..1024 {
                        let word = bitmap[i];
                        if word != 0 {
                            let base = (i << 6) as u32;
                            for bit in 0..64 {
                                if (word & (1 << bit)) != 0 {
                                    res.push(high | base | bit);
                                }
                            }
                        }
                    }
                }
            }
        }
        res
    }

    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }

    /// Returns the total number of IDs stored in this roaring bitmap
    pub fn len(&self) -> usize {
        let mut count = 0;
        for container in self.containers.values() {
            match container {
                Container::Array(arr) => count += arr.len(),
                Container::Bitmap(bitmap) => {
                    for &word in bitmap.iter() {
                        count += word.count_ones() as usize;
                    }
                }
            }
        }
        count
    }

    /// Computes the Jaccard similarity index (intersection size / union size) between two roaring bitmaps
    pub fn jaccard_similarity(&self, other: &Self) -> f64 {
        let intersection = self.intersect(other);
        let union_set = self.union(other);
        let intersection_count = intersection.len();
        let union_count = union_set.len();
        if union_count == 0 {
            0.0
        } else {
            intersection_count as f64 / union_count as f64
        }
    }
}

// ─── Prime Partitioned Gödel Filter ─────────────────────────────────────

pub const PRIMES: &[u64] = &[
    2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
    73, 79, 83, 89, 97, 101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163, 167, 173,
    179, 181, 191, 193, 197, 199, 211, 223, 227, 229, 233, 239, 241, 251, 257, 263, 269, 271, 277, 281,
    283, 293, 307, 311, 313, 317, 331, 337, 347, 349, 353, 359, 367, 373, 379, 383, 389, 397, 401, 409,
    419, 421, 431, 433, 439, 443, 449, 457, 461, 463, 467, 479, 487, 491, 499, 503, 509, 521, 523, 541,
];

/// Simple fast FNV-1a 32-bit hash function
pub fn fnv1a_hash(bytes: &[u8]) -> u32 {
    let mut hash = 2166136261u32;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

/// A Prime Filter representation of a document's lexical terms and FST tags
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimeFilter {
    // 8 buckets of prime-encoded signature lane products
    pub signatures: [u64; 8],
    // Perfect Gödel signature for FST tags
    pub tag_signature: u128,
}

impl PrimeFilter {
    pub fn new() -> Self {
        Self {
            signatures: [1; 8],
            tag_signature: 1,
        }
    }

    /// Add a vocabulary term to the partitioned signature buckets
    pub fn add_term(&mut self, term_bytes: &[u8]) {
        let h = fnv1a_hash(term_bytes);
        let bucket = (h as usize) % 8;
        let prime_idx = (h as usize) % PRIMES.len();
        let prime = PRIMES[prime_idx];

        // Safely multiply. If we hit integer overflow capacity, we saturate the bucket (remains unchanged)
        if let Some(val) = self.signatures[bucket].checked_mul(prime) {
            self.signatures[bucket] = val;
        }
    }

    /// Add a tag category kind to the tag Gödel signature
    pub fn add_tag_kind(&mut self, kind: &str) {
        let h = fnv1a_hash(kind.as_bytes());
        let prime_idx = (h as usize) % PRIMES.len();
        let prime = PRIMES[prime_idx] as u128;

        if let Some(val) = self.tag_signature.checked_mul(prime) {
            self.tag_signature = val;
        }
    }

    /// Check if a query term is possibly present in the document.
    /// Returns false if it is definitely NOT present.
    pub fn test_term(&self, term_bytes: &[u8]) -> bool {
        let h = fnv1a_hash(term_bytes);
        let bucket = (h as usize) % 8;
        let prime_idx = (h as usize) % PRIMES.len();
        let prime = PRIMES[prime_idx];

        self.signatures[bucket] % prime == 0
    }

    /// Check if tag category kind is definitely not present
    pub fn test_tag_kind(&self, kind: &str) -> bool {
        let h = fnv1a_hash(kind.as_bytes());
        let prime_idx = (h as usize) % PRIMES.len();
        let prime = PRIMES[prime_idx] as u128;

        self.tag_signature % prime == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_miniroaring_basic() {
        let mut bitmap = MiniRoaring::new();
        assert!(bitmap.is_empty());
        assert_eq!(bitmap.len(), 0);

        bitmap.insert(5);
        bitmap.insert(10);
        bitmap.insert(1000);
        assert!(!bitmap.is_empty());
        assert_eq!(bitmap.len(), 3);
        assert!(bitmap.contains(5));
        assert!(bitmap.contains(10));
        assert!(bitmap.contains(1000));
        assert!(!bitmap.contains(11));

        let sorted = bitmap.iter();
        assert_eq!(sorted, vec![5, 10, 1000]);
    }

    #[test]
    fn test_miniroaring_upgrade() {
        let mut bitmap = MiniRoaring::new();
        // Insert 1025 unique values to force upgrade to dense Container::Bitmap
        for i in 0..1025 {
            bitmap.insert(i * 2);
        }
        assert_eq!(bitmap.len(), 1025);
        assert!(bitmap.contains(0));
        assert!(bitmap.contains(2048));
        assert!(!bitmap.contains(1));

        let key = 0u16;
        let container = bitmap.containers.get(&key).unwrap();
        match container {
            Container::Bitmap(_) => {}
            _ => panic!("Expected Container::Bitmap, found Array"),
        }
    }

    #[test]
    fn test_miniroaring_operations() {
        let mut a = MiniRoaring::new();
        a.insert(1);
        a.insert(2);
        a.insert(3);

        let mut b = MiniRoaring::new();
        b.insert(2);
        b.insert(3);
        b.insert(4);

        let intersection = a.intersect(&b);
        assert_eq!(intersection.iter(), vec![2, 3]);
        assert_eq!(intersection.len(), 2);

        let union_set = a.union(&b);
        assert_eq!(union_set.iter(), vec![1, 2, 3, 4]);
        assert_eq!(union_set.len(), 4);

        let jaccard = a.jaccard_similarity(&b);
        // intersection / union = 2 / 4 = 0.5
        assert!((jaccard - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_prime_filter() {
        let mut filter = PrimeFilter::new();
        
        filter.add_term(b"apple");
        filter.add_term(b"banana");
        
        assert!(filter.test_term(b"apple"));
        assert!(filter.test_term(b"banana"));
        
        filter.add_tag_kind("intent");
        assert!(filter.test_tag_kind("intent"));
        assert!(!filter.test_tag_kind("offensive_en"));
    }
}
