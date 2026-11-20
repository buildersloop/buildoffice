//! 秘密の値の包み。表示・写し・比べることをさせない（ログやエラーの文に、うっかり入らないように）。

use std::fmt;

/// 秘密の値（鍵・解いた値・招待の秘密・短い数字・合言葉）の包み。値を作るタスクで包む。
///
/// - `Debug` は `Secret(..)` だけを書く。`Display`・`Clone`・`PartialEq`・`Hash`・`Deref`・`AsRef`・`Borrow`・
///   `From<Secret<T>> for T` は実装しない。中身を使うときは [`Secret::expose`] か [`Secret::into_inner`] を
///   名指しで呼ぶ（レビューで探せる）。
/// - 捨てるときに 0 で埋める処理は、まだない（置き場所は、あとで決める）。
///
/// ```
/// use safe_log::Secret;
/// let key = Secret::new("C:\\Profiles\\alice");
/// assert_eq!(format!("{key:?}"), "Secret(..)");
/// assert_eq!(*key.expose(), "C:\\Profiles\\alice");
/// assert_eq!(key.into_inner(), "C:\\Profiles\\alice");
/// ```
///
/// 表示はできない：
///
/// ```compile_fail
/// let text = format!("{}", safe_log::Secret::new(1));
/// ```
///
/// 写せない・比べられない・中身として扱えない：
///
/// ```compile_fail
/// let a = safe_log::Secret::new(1);
/// let b = a.clone();
/// ```
///
/// ```compile_fail
/// let same = safe_log::Secret::new(1) == safe_log::Secret::new(1);
/// ```
///
/// ```compile_fail
/// let n: u64 = *safe_log::Secret::new(1u64);
/// ```
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// 包む。
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// 中身を借りる。
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// 包みを外して、中身を返す。
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        for text in [
            format!("{:?}", Secret::new(r"C:\Profiles\SECRET-USER-q7")),
            format!("{:#?}", Secret::new(12_345_678_u64)),
            format!("{:?}", Secret::new(vec![0xde_u8, 0xad, 0xbe, 0xef])),
            format!("{:?}", Some(Secret::new("ABCD-EFGH-JKLM-NPQR"))),
        ] {
            assert!(!text.contains("SECRET"), "{text}");
            assert!(!text.contains("12345678"), "{text}");
            assert!(!text.contains("222"), "{text}");
            assert!(!text.contains("ABCD"), "{text}");
            assert!(text.contains("Secret(..)"), "{text}");
        }
    }
}
