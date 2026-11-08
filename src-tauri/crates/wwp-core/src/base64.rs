//! 最小 base64 编码器——只为把缩略图塞进 `data:` URL。
//!
//! ## 为什么不引入 `base64` crate
//!
//! 这个项目的约定是"能不加依赖就不加，先看标准库与已有依赖里有没有"
//! （单实例门闩与哈希都是这么处理的：自己写 FNV-1a 而不是引入哈希库）。
//! 这里要的只是**编码**、一个方向，标准字母表 + 补齐，二十行就够；
//! 引一个 crate 只会让交付产物的依赖树更长。
//!
//! 解码不实现：没有任何调用点需要它，写了就是死代码。

/// 标准 base64 字母表（RFC 4648，含 `+/`，末尾补齐 `=`）。
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 编码成 base64 字符串。
pub fn encode(bytes: &[u8]) -> String {
    // 每 3 字节 → 4 个字符，因此容量一次算准，避免反复重新分配。
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        // 最后一个字节不参与后两组时补 `=`。
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 §10 的样例向量——比自己编的例子可信。
    #[test]
    fn matches_the_rfc_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn length_is_always_a_multiple_of_four() {
        // 补齐写错时长度会不是 4 的倍数——那是 base64 最容易错的一处。
        for len in 0..40usize {
            let bytes = vec![0xA5u8; len];
            let encoded = encode(&bytes);
            assert_eq!(encoded.len() % 4, 0, "长度 {len} 时补齐不对：{encoded}");
        }
    }

    #[test]
    fn covers_the_whole_alphabet_including_plus_and_slash() {
        // 0xFB/0xFF 这类字节会用到 `+` 与 `/`：字母表写错时它们会变成别的字符。
        let encoded = encode(&[0xfb, 0xff, 0xbf]);
        assert_eq!(encoded, "+/+/");
    }
}
