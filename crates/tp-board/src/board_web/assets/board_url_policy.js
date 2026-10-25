// Browser port of plan_markup.rs `allowed_url`/`blocked_authority`: the one
// link-destination policy shared by sanitized plan markup and linkified text.
// tests/support/board_url_policy_vectors.json holds the vectors both the Rust
// and the Node suites assert, so the two implementations cannot drift.
const UNSAFE_URL_CHARACTER = /[\p{Cc}\p{White_Space}\\]/u;

function asciiLower(value) { return value.replace(/[A-Z]/g, letter => letter.toLowerCase()); }
function isAscii(value) { return /^[\x00-\x7f]*$/.test(value); }
function hexValue(code) {
  if (code >= 0x30 && code <= 0x39) return code - 0x30;
  if (code >= 0x61 && code <= 0x66) return code - 0x61 + 10;
  if (code >= 0x41 && code <= 0x46) return code - 0x41 + 10;
  return null;
}

// Decodes `%XX` byte escapes in one pass; malformed or trailing `%` stays
// literal, and invalid UTF-8 decodes lossily (as Rust's from_utf8_lossy).
function percentDecode(value) {
  if (!value.includes("%")) return value;
  const bytes = new TextEncoder().encode(value);
  const decoded = new Uint8Array(bytes.length);
  let length = 0;
  for (let index = 0; index < bytes.length;) {
    let byte = bytes[index], consumed = 1;
    if (byte === 0x25 && index + 2 < bytes.length) {
      const high = hexValue(bytes[index + 1]), low = hexValue(bytes[index + 2]);
      if (high !== null && low !== null) { byte = high << 4 | low; consumed = 3; }
    }
    decoded[length++] = byte;
    index += consumed;
  }
  return new TextDecoder().decode(decoded.subarray(0, length));
}

// Decimal digits (octal spellings included) or `0x` plus hex digits; a bare
// `0x` counts as zero in URL host parsing.
function isNumericLabel(label) {
  if (/^0x/i.test(label)) return /^[0-9a-fA-F]*$/.test(label.slice(2));
  return /^[0-9]+$/.test(label);
}
function isNumericIpv4(host) { return host !== "" && host.split(".").every(isNumericLabel); }

// True for `localhost`, `*.localhost` (one trailing dot tolerated), non-ASCII
// hosts, and IP literals, after percent-decoding and stripping userinfo/port.
export function boardAuthorityBlocked(authority) {
  const decoded = percentDecode(authority);
  const host = decoded.slice(decoded.lastIndexOf("@") + 1);
  if (host.startsWith("[") || !isAscii(host)) return true;
  let bare = host.split(":")[0];
  if (bare.endsWith(".")) bare = bare.slice(0, -1);
  const lower = asciiLower(bare);
  return lower === "localhost" || lower.endsWith(".localhost") || isNumericIpv4(bare);
}

// HTTP(S) and mailto survive; among relative links only same-page `#…`
// anchors do. Controls, whitespace, backslashes, `//`, a `token=` fragment,
// and absolute links to localhost or IP literals are denied.
export function boardUrlAllowed(url) {
  const text = String(url ?? "");
  if (UNSAFE_URL_CHARACTER.test(text) || text.startsWith("//")) return false;
  const hash = text.indexOf("#");
  if (hash >= 0 && text.slice(hash + 1).startsWith("token=")) return false;
  const prefix = text.split(/[/?#]/)[0];
  const colon = prefix.indexOf(":");
  if (colon < 0) return text.startsWith("#");
  const scheme = asciiLower(prefix.slice(0, colon)), rest = text.slice(colon + 1);
  if (scheme === "mailto") return rest !== "";
  if (scheme !== "http" && scheme !== "https") return false;
  if (!rest.startsWith("//")) return false;
  const authority = rest.slice(2).split(/[/?#]/)[0];
  return authority !== "" && !boardAuthorityBlocked(authority);
}
