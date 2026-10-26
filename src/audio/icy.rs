/// Incremental ICY demultiplexer; handles metadata split at arbitrary HTTP chunk boundaries.
pub struct Icy {
    interval: usize,
    audio_left: usize,
    metadata_left: usize,
    metadata: Vec<u8>,
    need_length: bool,
}
impl Icy {
    pub fn new(interval: usize) -> Self {
        Self {
            interval,
            audio_left: interval,
            metadata_left: 0,
            metadata: Vec::with_capacity(4080),
            need_length: false,
        }
    }
    pub fn consume<'a>(&mut self, input: &mut &'a [u8]) -> (Option<&'a [u8]>, Option<String>) {
        if input.is_empty() {
            return (None, None);
        }
        if self.interval == 0 {
            let data = *input;
            *input = &[];
            return (Some(data), None);
        }
        if self.audio_left > 0 {
            let n = self.audio_left.min(input.len());
            let data = &input[..n];
            *input = &input[n..];
            self.audio_left -= n;
            if self.audio_left == 0 {
                self.need_length = true;
            }
            return (Some(data), None);
        }
        if self.need_length {
            self.metadata_left = input[0] as usize * 16;
            *input = &input[1..];
            self.metadata.clear();
            self.need_length = false;
            if self.metadata_left == 0 {
                self.audio_left = self.interval;
            }
            return (None, None);
        }
        let n = self.metadata_left.min(input.len());
        self.metadata.extend_from_slice(&input[..n]);
        *input = &input[n..];
        self.metadata_left -= n;
        if self.metadata_left == 0 {
            self.audio_left = self.interval;
            let text = String::from_utf8_lossy(&self.metadata);
            let title = text
                .split("StreamTitle='")
                .nth(1)
                .and_then(|v| v.split("';").next())
                .map(|v| v.chars().filter(|c| !c.is_control()).take(256).collect());
            return (None, title);
        }
        (None, None)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_metadata() {
        let mut icy = Icy::new(3);
        let mut data = b"abc\x01StreamTitle='x';def".as_slice();
        let mut audio = Vec::new();
        let mut titles = Vec::new();
        while !data.is_empty() {
            let mut b = &data[..1];
            let (a, t) = icy.consume(&mut b);
            if let Some(a) = a {
                audio.extend_from_slice(a);
            }
            if let Some(t) = t {
                titles.push(t);
            }
            data = &data[1..];
        }
        assert_eq!(audio, b"abcdef");
        assert_eq!(titles, vec!["x"]);
    }
}
