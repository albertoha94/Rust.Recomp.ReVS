//! Groups functions into the game's subsystems by their (demangled) name, for
//! the summary and so later phases can pick a subsystem to work on.

/// Returns the subsystem a function belongs to, from its demangled name
/// (e.g. "GM_CtrlMain_Player::Update()") or raw symbol name. `None` = no name.
/// The first matching rule wins, so specific prefixes come before general ones.
pub fn classify(name: Option<&str>) -> &'static str {
    let Some(n) = name else { return "unnamed" };
    // Raw C symbols carry Mach-O's leading underscore.
    let c = n.strip_prefix('_').unwrap_or(n);
    let starts = |p: &[&str]| p.iter().any(|x| n.starts_with(x) || c.starts_with(x));

    // Objective-C: ads/store extras to drop, then the app shell.
    if n.starts_with("-[") || n.starts_with("+[") {
        return if n.contains("Cloudia") || n.contains("Movie") || n.contains("Receipt") || n.contains("Store") {
            "drop:objc"
        } else {
            "objc_app"
        };
    }
    // Game: the HUD separately (frame counters matter for 60 fps), then the rest.
    if starts(&["GM_CtrlLyt_"]) {
        return "gm_hud";
    }
    if starts(&["GM_"]) {
        return "gm_match";
    }
    if starts(&["BioVS_", "IBioVS_"]) {
        return "app";
    }
    if starts(&["Bio4_"]) {
        return "bio4";
    }
    if starts(&["LYT_", "SLYT_", "script::", "LAS_", "STED_"]) {
        return "menus";
    }
    if starts(&["net::", "gkcpp::"]) {
        return "network";
    }
    if starts(&["rcx::"]) {
        return "engine";
    }
    // The 3D engine that reads the .mcms/.mcts/.mcas files (mceLoader,
    // mceBoneDeformer, mceGraphics3D...).
    if starts(&["mce"]) {
        return "engine3d";
    }
    if starts(&["CGC_", "CT_", "CMN_", "ERP_", "ct::"]) {
        return "system";
    }
    if starts(&["STORE_", "Cloudia"]) {
        return "drop:store";
    }
    // Bundled libraries.
    if starts(&[
        "std::", "__gnu_cxx::", "__cxxabiv1::", "__cxa_", "__gxx_", "operator new", "operator delete",
        "typeinfo", "vtable for", "__static_initialization", "__GLOBAL__", "__tcf_",
    ]) {
        return "lib:c++";
    }
    if starts(&[
        "SSL", "ssl", "EVP_", "BN_", "RSA_", "DSA_", "DH_", "EC_", "ECDSA_", "ECDH_", "X509", "x509", "PEM_", "ASN1_",
        "asn1", "d2i_", "i2d_", "BIO_", "ERR_", "CRYPTO_", "OBJ_", "RAND_", "SHA", "MD5", "MD4", "HMAC", "AES_", "DES_",
        "RC4", "BF_", "CAST_", "PKCS", "OPENSSL", "CONF_", "ENGINE_", "UI_", "lh_", "sk_", "COMP_", "X9_", "v2i_", "i2v_",
        "i2s_", "s2i_", "NCONF_", "OCSP_", "TS_", "CMS_", "EVP", "NETSCAPE_", "BUF_", "OBJ", "dtls1", "tls1", "ssl3",
    ]) {
        return "lib:openssl";
    }
    if starts(&["inflate", "deflate", "crc32", "adler32", "zlib", "zError", "zcalloc", "zcfree", "_tr_", "unz", "zip"]) {
        return "lib:zlib";
    }
    "other"
}

/// OpenSSL is linked as one block, but many of its internal functions have
/// short lowercase names the prefix rules can't tell apart from other code.
/// Functions between the first and last name-matched OpenSSL function that no
/// rule claimed (`other` / `unnamed`) are OpenSSL too.
pub fn claim_library_block(funcs: &mut [(u32, &'static str)], lib: &'static str) {
    let hits: Vec<u32> = funcs.iter().filter(|f| f.1 == lib).map(|f| f.0).collect();
    let (Some(&lo), Some(&hi)) = (hits.iter().min(), hits.iter().max()) else { return };
    for f in funcs.iter_mut() {
        if f.0 >= lo && f.0 <= hi && (f.1 == "other" || f.1 == "unnamed") {
            f.1 = lib;
        }
    }
}
