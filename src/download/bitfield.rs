use std::fmt::Display;

pub fn is_seed(total_pieces: usize, bitfield: &[u8]) -> bool {
    if bitfield.len() > 1 {
        // Iterate over all bytes except the last one.
        for idx in 0..bitfield.len() - 1 {
            if bitfield[idx] != 255 {
                return false;
            }
        }
    }

    let last_idx = (bitfield.len() - 1) * 8;
    for idx in last_idx..total_pieces {
        let (bucket, offset) = piece_bucket_offset(idx as u32);
        if bitfield[bucket] & 1 << offset == 0 {
            return false;
        }
    }
    true
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bitfield {
    /// A bitvec which indicates which pieces we own.
    piece_ownership: Vec<u8>,

    /// The number of pieces that the peer owns.
    /// Since we're only downloading from seeds
    /// all the bits will be set to 1 and we don't have
    /// to iterate through the bitfield to count the 1s.
    total_pieces: usize,

    /// The next piece index which we don't own.
    cur: usize,

    /// We own all the pieces up to this index.
    confirmed: usize,
}

impl Display for Bitfield {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for bits in &self.piece_ownership {
            write!(f, "{:08b} ", bits)?;
        }
        Ok(())
    }
}

impl Bitfield {
    pub fn from_vec(total_pieces: usize, v: Vec<u8>) -> Self {
        Self {
            piece_ownership: v,
            total_pieces,
            cur: 0,
            confirmed: 0,
        }
    }

    pub fn zero(num_pieces: u32) -> Self {
        let len = (num_pieces as usize + 7) / 8;
        Self {
            piece_ownership: vec![0; len],
            total_pieces: num_pieces as usize,
            cur: 0,
            confirmed: 0,
        }
    }

    pub fn pick_piece(&mut self) -> Option<u32> {
        // Check if we already own all the pieces.
        if self.cur >= self.total_pieces && self.confirmed >= self.total_pieces {
            return None;
        }

        while self.confirmed < self.total_pieces {
            let bit_index = self.cur as u32;
            self.cur += 1;
            if self.cur == self.total_pieces {
                self.cur = self.confirmed;
            }
            // Skip if we already have the piece.
            if !self.has(bit_index) {
                return Some(bit_index);
            }
        }
        None
    }

    pub fn has(&self, piece_idx: u32) -> bool {
        let (bucket, offset) = piece_bucket_offset(piece_idx);
        self.piece_ownership[bucket] & 1 << offset > 0
    }

    pub fn set(&mut self, piece_idx: u32) {
        let (bucket, offset) = piece_bucket_offset(piece_idx);
        self.piece_ownership[bucket] = self.piece_ownership[bucket] | 1 << offset;

        // Also advance confirmed as much as possible.
        if self.confirmed == piece_idx as usize {
            self.confirmed += 1;
            while self.has(self.confirmed as u32) {
                self.confirmed += 1;
            }
        }
    }

    pub fn remaining(&self) -> u32 {
        // Due to padding at the end, we can't count 0s.
        let num_owned = self
            .piece_ownership
            .iter()
            .fold(0, |acc, i| acc + i.count_ones());

        self.total_pieces as u32 - num_owned
    }
}

// fn total_pieces(v: &[u8]) -> usize {
//     // total number of bits to iterate through.
//     let total_bits = v.len() * 8;
//     // total number of bits which are set 1.
//     let mut set_bits = 0;
//
//     // while bit_index < total_bits {
//     for bit_index in 0..total_bits {
//         let (bucket, offset) = piece_bucket_offset(bit_index as u32);
//         if v[bucket] & 1 << offset > 0 {
//             set_bits += 1;
//         }
//     }
//     set_bits
// }

fn piece_bucket_offset(piece_idx: u32) -> (usize, u8) {
    let bucket = piece_idx / 8;
    let offset = 7 - piece_idx % 8;
    (bucket as usize, offset as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_seed() {
        let v1 = vec![0b11111111];
        assert!(is_seed(8, &v1));

        let v2 = vec![0b11111100];
        assert!(is_seed(6, &v2));

        let v3 = vec![0b11111111, 0b11000000];
        assert!(!is_seed(11, &v3));

        let v4 = vec![0b11111111, 0b11100000];
        assert!(is_seed(11, &v4));

        let v5 = vec![0b11111111, 0b11111110];
        assert!(!is_seed(16, &v5));

        let v6 = vec![0b11111111, 0b11111111];
        assert!(is_seed(16, &v6));
    }

    // #[test]
    // fn test_total_pieces() {
    //     let bitfield = vec![0b10101101, 0b11000000];
    //     assert_eq!(7, total_pieces(&bitfield[..]));
    // }

    #[test]
    fn test_has() {
        // torrent with 12 pieces:
        // 1010 1101 1100 0000

        let bitfield = Bitfield::from_vec(12, vec![0b10101101, 0b11000000]);
        println!("{bitfield}");
        // Has piece 0
        assert!(bitfield.has(0));
        // Doesn't have piece 1
        assert!(!bitfield.has(1));
        // Has piece 7
        assert!(bitfield.has(7));
        // Has piece 9
        assert!(bitfield.has(9));
        // Doesn't have piece 10
        assert!(!bitfield.has(10));
    }

    #[test]
    fn test_set() {
        //ownership vector with 8 pieces:
        // 0000 0000
        // |
        // conf
        let mut bitfield = Bitfield::from_vec(8, vec![0b00000000]);
        bitfield.confirmed = 0;

        bitfield.set(1);
        bitfield.set(2);
        bitfield.set(4);
        // At this point ownership is as follows:
        // 0110 1000
        // |
        // conf

        bitfield.set(0);
        assert_eq!(3, bitfield.confirmed);
        println!("{bitfield}");
        // At this point ownership is as follows:
        // 0110 1000
        //    |
        //   conf
    }

    #[test]
    fn test_pick_piece() {
        //ownership vector with 8 pieces:
        // 0000 0000
        // |      |
        // conf  curr
        let mut bitfield = Bitfield::from_vec(8, vec![0b00000000]);
        bitfield.cur = 6;
        bitfield.confirmed = 0;

        let mut next = 5;
        for _ in 0..100 {
            next = (next + 1) % 8;
            assert_eq!(Some(next), bitfield.pick_piece());
        }

        //ownership vector with 8 pieces:
        // 1111 1111 x
        //         | |
        //    curr-- --conf
        let mut bitfield = Bitfield::from_vec(8, vec![0b00000000]);
        bitfield.cur = 7;
        bitfield.confirmed = 8;

        assert_eq!(None, bitfield.pick_piece());
        assert_eq!(None, bitfield.pick_piece());
        assert_eq!(None, bitfield.pick_piece());

        //ownership vector with 12 pieces:
        // 0000 0000 1000 0000
        // |      |
        // curr  conf
        let mut bitfield = Bitfield::from_vec(12, vec![0b00000000, 0b10000000]);
        bitfield.cur = 6;
        bitfield.confirmed = 0;

        assert_eq!(Some(6), bitfield.pick_piece());
        assert_eq!(Some(7), bitfield.pick_piece());
        assert_eq!(Some(9), bitfield.pick_piece());

        //ownership vector with 8 pieces:
        //      cur
        //       |
        // 1100 1000
        //   |
        //  conf
        let mut bitfield = Bitfield::from_vec(8, vec![0b1100_1000]);
        bitfield.cur = 5;
        bitfield.confirmed = 2;

        assert_eq!(Some(5), bitfield.pick_piece());
        assert_eq!(Some(6), bitfield.pick_piece());
        assert_eq!(Some(7), bitfield.pick_piece());
        assert_eq!(Some(2), bitfield.pick_piece());
        assert_eq!(Some(3), bitfield.pick_piece());
        assert_eq!(Some(5), bitfield.pick_piece());

        //ownership vector with 8 pieces:
        //        cur
        //         |
        // 1111 0111
        //      |
        //     conf
        let mut bitfield = Bitfield::from_vec(8, vec![0b1111_0111]);
        bitfield.cur = 7;
        bitfield.confirmed = 4;

        assert_eq!(Some(4), bitfield.pick_piece());
        assert_eq!(Some(4), bitfield.pick_piece());
        assert_eq!(Some(4), bitfield.pick_piece());

        //ownership vector with 8 pieces:
        // cur
        // |
        // 1111 0111
        // |
        // cur
        let mut bitfield = Bitfield::from_vec(8, vec![0b0000_0000]);

        // Starts at 0.
        assert_eq!(Some(0), bitfield.pick_piece());
        assert_eq!(Some(1), bitfield.pick_piece());
        assert_eq!(Some(2), bitfield.pick_piece());
    }

    #[test]
    fn test_remaining() {
        let bitfield = Bitfield::from_vec(8, vec![0b11111111]);
        assert_eq!(0, bitfield.remaining());

        let bitfield = Bitfield::from_vec(8, vec![0b11111100]);
        assert_eq!(2, bitfield.remaining());

        let bitfield = Bitfield::from_vec(12, vec![0b11011011, 0b11000000]);
        assert_eq!(4, bitfield.remaining());

        let bitfield = Bitfield::zero(1931);
        assert_eq!(1931, bitfield.remaining());
    }
}
