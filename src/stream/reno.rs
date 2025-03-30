#[derive(Debug)]
pub struct Reno {
    cwnd: usize,
    min_cwnd: usize,
    ssthresh: usize,
    rwnd: usize,
}

impl Default for Reno {
    fn default() -> Self {
        Reno {
            cwnd: 1024 * 2,
            min_cwnd: 1024 * 2,
            ssthresh: usize::MAX,
            rwnd: 64 * 1024,
        }
    }
}

impl Reno {
    pub fn new(mss: usize) -> Self {
        Reno {
            min_cwnd: mss,
            ..Reno::default()
        }
    }
}

impl Reno {
    pub fn window(&self) -> usize {
        self.cwnd
    }

    pub fn on_ack(&mut self, len: usize) {
        let len = if self.cwnd < self.ssthresh {
            // Slow start.
            len
        } else {
            self.ssthresh = self.cwnd;
            self.min_cwnd
        };

        self.cwnd = self.cwnd.saturating_add(len).min(self.rwnd).max(self.min_cwnd);
    }

    pub fn on_duplicate_ack(&mut self) {
        self.ssthresh = (self.cwnd >> 1).max(self.min_cwnd);
    }

    pub fn on_retransmit(&mut self) {
        self.cwnd = (self.cwnd >> 1).max(self.min_cwnd);
    }

    #[allow(dead_code)]
    pub fn set_mss(&mut self, mss: usize) {
        self.min_cwnd = mss;
    }

    pub fn set_remote_window(&mut self, remote_window: usize) {
        if self.rwnd < remote_window {
            self.rwnd = remote_window;
        }
    }
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_reno() {
        let remote_window = 64 * 1024;

        for i in 0..10 {
            for j in 0..9 {
                let mut reno = Reno::new(1480);
                reno.set_mss(1480);

                // Set remote window.
                reno.set_remote_window(remote_window);

                reno.on_ack(4096);

                let mut n = i;
                for _ in 0..j {
                    n *= i;
                }

                if i & 1 == 0 {
                    reno.on_retransmit();
                } else {
                    reno.on_duplicate_ack();
                }

                reno.on_ack(n);

                let cwnd = reno.window();
                println!("Reno: elapsed = , cwnd = {}", cwnd);

                assert!(cwnd >= reno.min_cwnd);
                assert!(reno.window() <= remote_window);
            }
        }
    }

    #[test]
    fn reno_min_cwnd() {
        let remote_window = 64 * 1024;

        let mut reno = Reno::new(1480);
        reno.set_remote_window(remote_window);

        for _ in 0..100 {
            reno.on_retransmit();
            assert!(reno.window() >= reno.min_cwnd);
        }
    }

    #[test]
    fn reno_set_rwnd() {
        let mut reno = Reno::new(1480);
        reno.set_remote_window(64 * 1024 * 1024);

        println!("{reno:?}");
    }
}
