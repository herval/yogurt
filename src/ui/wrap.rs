use unicode_width::UnicodeWidthStr;

/// Word-wrap text to a display width (unicode-aware, unlike the Go version
/// which measured bytes).
pub fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for input_line in text.lines() {
        if input_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in input_line.split_whitespace() {
            if line.is_empty() {
                line = word.to_string();
            } else if UnicodeWidthStr::width(line.as_str()) + 1 + UnicodeWidthStr::width(word) <= width
            {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
            // Hard-break words longer than the width.
            while UnicodeWidthStr::width(line.as_str()) > width {
                let mut w = 0;
                let mut split = line.len();
                for (i, c) in line.char_indices() {
                    let cw = UnicodeWidthStr::width(c.to_string().as_str());
                    if w + cw > width {
                        split = i;
                        break;
                    }
                    w += cw;
                }
                if split == 0 || split >= line.len() {
                    break;
                }
                let rest = line.split_off(split);
                out.push(std::mem::take(&mut line));
                line = rest;
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Truncate to a display width, appending … when cut.
pub fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = UnicodeWidthStr::width(c.to_string().as_str());
        if w + cw > width.saturating_sub(1) {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    out
}
