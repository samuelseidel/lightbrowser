//! Chrome's User-Agent Client Hints brand list: the real brands, the GREASE
//! brand and their order, derived from the Chrome major version.
//!
//! This reproduces Chromium's brand generation from memory of
//! `GenerateBrandVersionList` / `GetGreasedUserAgentBrandVersion` in
//! `components/embedder_support/user_agent_utils.cc` (the function names and
//! path are from memory and unverified; no Chromium source was available).
//! The seed is the Chrome major version `N`:
//!
//! - GREASE brand = `"Not" + C[N % 11] + "A" + C[(N + 1) % 11] + "Brand"`
//!   with `C = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"]`.
//! - GREASE version = `["8", "99", "24"][N % 3]` (full version `<v>.0.0.0`).
//! - Positions of (GREASE, Chromium, Google Chrome) in the list come from
//!   `[[0,1,2],[0,2,1],[1,0,2],[1,2,0],[2,0,1],[2,1,0]][N % 6]`.
//!
//! Checked against brand lists remembered from real Chrome: 124
//! (`Not-A.Brand` 99), 130 (`Not?A_Brand` 99, order Chromium, Chrome, GREASE),
//! 131 (`Not_A Brand` 24, order Chrome, Chromium, GREASE) and 153
//! (`Not_A Brand` 8, matching `Persona::default_chrome()`).
//!
//! Not verified: the older 8-brand GREASE table (`Not A(Brand`, `Not)A;Brand`,
//! ...) that Chromium used before this scheme; it does not reproduce the
//! data points above, so it is not implemented. Behaviour for majors below
//! about 110, and for Chromium builds without the `Google Chrome` brand
//! (2-entry lists), is not modelled.

use crate::profile::BrandVersion;

const GREASY_CHARS: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
const GREASY_VERSIONS: [&str; 3] = ["8", "99", "24"];
const ORDERS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

/// The GREASE brand name and its significant version for a Chrome major.
pub fn grease_brand(major: u32) -> (String, &'static str) {
    let n = major as usize;
    let brand = format!(
        "Not{}A{}Brand",
        GREASY_CHARS[n % GREASY_CHARS.len()],
        GREASY_CHARS[(n + 1) % GREASY_CHARS.len()]
    );
    (brand, GREASY_VERSIONS[n % GREASY_VERSIONS.len()])
}

fn build(major: u32, chrome_v: &str, chromium_v: &str, grease_v: &str) -> Vec<BrandVersion> {
    let (grease, _) = grease_brand(major);
    let order = ORDERS[major as usize % ORDERS.len()];
    let mut list: [Option<BrandVersion>; 3] = [None, None, None];
    list[order[0]] = Some(BrandVersion::new(grease, grease_v));
    list[order[1]] = Some(BrandVersion::new("Chromium", chromium_v));
    list[order[2]] = Some(BrandVersion::new("Google Chrome", chrome_v));
    list.into_iter().flatten().collect()
}

/// `navigator.userAgentData.brands` / `sec-ch-ua` for Google Chrome `major`.
pub fn chrome_brands(major: u32) -> Vec<BrandVersion> {
    let (_, gv) = grease_brand(major);
    let m = major.to_string();
    build(major, &m, &m, gv)
}

/// The full-version brand list (`sec-ch-ua-full-version-list`) for Google
/// Chrome `major` whose full version is `full_version` (`153.0.8010.12`).
pub fn chrome_full_version_list(major: u32, full_version: &str) -> Vec<BrandVersion> {
    let (_, gv) = grease_brand(major);
    build(major, full_version, full_version, &format!("{gv}.0.0.0"))
}

/// Major version from a UA string token such as `Chrome/153.0.0.0`.
pub fn ua_major(user_agent: &str, token: &str) -> Option<u32> {
    let rest = &user_agent[user_agent.find(token)? + token.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format_brand_list;

    fn s(major: u32) -> String {
        format_brand_list(&chrome_brands(major))
    }

    #[test]
    fn matches_remembered_real_chrome_lists() {
        // 124 % 6 = 4 -> [2, 0, 1]: Chromium, Google Chrome, GREASE.
        assert_eq!(
            s(124),
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#
        );
        assert_eq!(
            s(130),
            r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#
        );
        assert_eq!(
            s(131),
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#
        );
    }

    #[test]
    fn chrome_153_matches_default_chrome() {
        let p = crate::Persona::default_chrome();
        assert_eq!(Some(s(153)), p.sec_ch_ua);
    }

    #[test]
    fn full_version_list_uses_grease_dot_zero() {
        let l = chrome_full_version_list(153, "153.0.8010.12");
        assert_eq!(
            format_brand_list(&l),
            r#""Google Chrome";v="153.0.8010.12", "Not_A Brand";v="8.0.0.0", "Chromium";v="153.0.8010.12""#
        );
    }

    #[test]
    fn ua_major_parses() {
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";
        assert_eq!(ua_major(ua, "Chrome/"), Some(153));
        assert_eq!(ua_major(ua, "Firefox/"), None);
    }
}
