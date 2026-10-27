//! In-process bridge clients for Blender, Unity and Substance Painter.
//!
//! The Python relays (`bridge/*.py`) existed only because a browser cannot open
//! raw sockets; this native binary talks to the targets directly — Blender over
//! its MCP addon socket (`:9876`), Unity and Painter over their local HTTP APIs.
//! Invariant: every call here blocks, so callers must run it on a worker thread.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use base64::Engine;
use serde_json::json;

use crate::model::{Item, Kind};

const HEALTH_TIMEOUT: Duration = Duration::from_millis(2000);
const SEND_TIMEOUT: Duration = Duration::from_secs(900);

/// Blender MCP addon socket.
pub const BLENDER_ADDON: &str = "127.0.0.1:9876";
/// Blender executable for the "open in a new window" action.
const BLENDER_EXE: &str = r"D:\SteamLibrary\steamapps\common\Blender\blender.exe";
/// Unity MCP config file (host + bearer token).
const UNITY_CONFIG: &str = r"C:\Users\antar\Documents\Unity Projects\INE_PROJ1\UserSettings\AI-Game-Developer-Config.json";

/// Bridge status for a header dot / dialog line.
#[derive(Clone, Default)]
pub struct Dot {
    /// `"ok"` | `"warn"` | `"down"`.
    pub state: String,
    pub text: String,
}

fn dot(state: &str, text: &str) -> Dot {
    Dot {
        state: state.to_string(),
        text: text.to_string(),
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
}

fn http_get(url: &str, timeout: Duration) -> Result<(u16, String), String> {
    let resp = agent(timeout).get(url).call().map_err(|e| e.to_string())?;
    let status = resp.status();
    resp.into_string()
        .map(|body| (status, body))
        .map_err(|e| e.to_string())
}

fn http_post(
    url: &str,
    body: &serde_json::Value,
    token: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let mut req = agent(timeout).post(url).set("Content-Type", "application/json");
    if let Some(t) = token {
        if !t.is_empty() {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
    }
    let resp = req.send_json(body.clone()).map_err(|e| e.to_string())?;
    let status = resp.status();
    resp.into_string()
        .map(|b| (status, b))
        .map_err(|e| e.to_string())
}

/// Absolute Windows path for a library-relative posix/backslash path.
fn abs_win(root: &str, rel: &str) -> String {
    let root = root.trim_end_matches(['\\', '/']);
    let tail = rel.replace('/', "\\");
    let tail = tail.trim_start_matches('\\');
    format!("{root}\\{tail}")
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/* ------------------------------- Blender -------------------------------- */

/// Any successful TCP connect means the port is listening.
fn tcp_alive(addr: &str, timeout: Duration) -> bool {
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    match addrs.next() {
        Some(a) => TcpStream::connect_timeout(&a, timeout).is_ok(),
        None => false,
    }
}

/// One NUL-terminated JSON request to the Blender MCP addon, one reply.
fn blender_execute(addr: &str, code: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let payload = json!({ "type": "execute", "code": code, "strict_json": true }).to_string();
    let mut stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let mut buf = payload.into_bytes();
    buf.push(0);
    stream.write_all(&buf).map_err(|e| e.to_string())?;

    let mut out: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.contains(&0) {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
    if end == 0 {
        return Err("Blender addon did not respond".to_string());
    }
    serde_json::from_slice(&out[..end]).map_err(|e| e.to_string())
}


/// Python helper injected before the per-asset import loop. It reads the
/// library sidecar (`<slug>.json`) and wires its material texture slots into
/// the FBX materials Blender created; the FBX itself often has stale texture
/// paths, which is why importing it alone looked untextured.
const BLENDER_TEXTURE_HELPER: &str = r#"
def _alb_texture(spec_path, root, fallbacks):
    import json as _json
    if not os.path.isfile(spec_path):
        return
    try:
        with open(spec_path, encoding='utf-8-sig') as _fh:
            _spec = _json.load(_fh)
    except Exception:
        return

    def _resolve(rel):
        if not rel:
            return None
        path = rel
        if not os.path.isabs(path):
            path = os.path.join(root, rel.replace('/', os.sep))
        if os.path.isfile(path):
            return path
        if '_opq' in path:
            alt = path.replace('_opq', '')
            if os.path.isfile(alt):
                return alt
        return None

    def _fallback(kind):
        for path in fallbacks:
            name = os.path.basename(path).lower()
            if kind == 'base' and any(k in name for k in ('main', '_alb', 'albedo', 'diffuse', 'basecolor')):
                return path
            if kind == 'normal' and any(k in name for k in ('_nrm', 'normal', '_n.', 'nmap')):
                return path
        return None

    def _image_node(mat, rel, kind):
        path = _resolve(rel)
        if path is None:
            path = _fallback(kind)
        if not path or not os.path.isfile(path):
            return None
        node = mat.node_tree.nodes.new('ShaderNodeTexImage')
        node.image = bpy.data.images.load(path, check_existing=True)
        node.label = os.path.basename(path)
        if kind == 'normal':
            node.image.colorspace_settings.name = 'Non-Color'
        return node

    def _material(name):
        low = (name or '').lower()
        if not low:
            return None
        for mat in bpy.data.materials:
            candidate = mat.name.lower()
            if candidate == low or candidate.startswith(low + '.'):
                return mat
        return None

    def _wire(mat, entry):
        mat.use_nodes = True
        tree = mat.node_tree
        # The FBX importer may already have created stale/duplicate texture
        # nodes; replace them with the library sidecar's explicit slots.
        for node in list(tree.nodes):
            if node.type in ('TEX_IMAGE', 'NORMAL_MAP'):
                tree.nodes.remove(node)
        bsdf = next((n for n in tree.nodes if n.type == 'BSDF_PRINCIPLED'), None)
        if bsdf is None:
            bsdf = tree.nodes.new('ShaderNodeBsdfPrincipled')
            out = next((n for n in tree.nodes if n.type == 'OUTPUT_MATERIAL'), None)
            if out is None:
                out = tree.nodes.new('ShaderNodeOutputMaterial')
            tree.links.new(bsdf.outputs['BSDF'], out.inputs['Surface'])

        textures = entry.get('textures') or {}
        params = entry.get('params') or {}
        if textures.get('base_color'):
            node = _image_node(mat, textures.get('base_color'), 'base')
            if node is not None:
                tree.links.new(node.outputs['Color'], bsdf.inputs['Base Color'])
        if textures.get('normal'):
            node = _image_node(mat, textures.get('normal'), 'normal')
            if node is not None:
                nmap = tree.nodes.new('ShaderNodeNormalMap')
                strength = params.get('normal_strength', 1.0)
                try:
                    nmap.inputs['Strength'].default_value = float(strength)
                except Exception:
                    pass
                tree.links.new(node.outputs['Color'], nmap.inputs['Color'])
                tree.links.new(nmap.outputs['Normal'], bsdf.inputs['Normal'])
        if 'metallic' in params:
            try:
                bsdf.inputs['Metallic'].default_value = float(params['metallic'])
            except Exception:
                pass
        if 'smoothness' in params:
            try:
                bsdf.inputs['Roughness'].default_value = max(0.0, min(1.0, 1.0 - float(params['smoothness'])))
            except Exception:
                pass
        if entry.get('double_sided') is not None:
            mat.use_backface_culling = not bool(entry.get('double_sided'))

    used = set()
    for obj in bpy.context.selected_objects:
        if obj.type == 'MESH':
            for slot in obj.material_slots:
                if slot.material is not None:
                    used.add(slot.material)

    applied = 0
    for entry in _spec.get('materials', []):
        mat = _material(entry.get('name'))
        if mat is not None:
            _wire(mat, entry)
            applied += 1
    if applied == 0:
        fallback_entry = {'textures': {}}
        base = _fallback('base')
        normal = _fallback('normal')
        if base:
            fallback_entry['textures']['base_color'] = base
        if normal:
            fallback_entry['textures']['normal'] = normal
        for mat in used:
            _wire(mat, fallback_entry)
"#;

/// Python run inside Blender: clear the scene, then import each FBX and bind
/// the asset into `scene.assetlib` so the Asset Library Bridge panel shows it.
pub(crate) fn blender_import_code(items: &[Item], root: &str) -> String {
    let root_w = root.replace('/', "\\");
    let mut lines = vec![
        "import bpy".to_string(),
        "import os".to_string(),
        BLENDER_TEXTURE_HELPER.to_string(),
        "imported = []".to_string(),
        "errors = []".to_string(),
        "bound = []".to_string(),
        "bind_errors = []".to_string(),
        "scene = bpy.context.scene".to_string(),
        "for _o in list(bpy.data.objects):".to_string(),
        "    bpy.data.objects.remove(_o, do_unlink=True)".to_string(),
    ];
    for it in items {
        let fbx = abs_win(root, &it.fbx_rel());
        let rel = it.path().replace('/', "\\");
        let slug = &it.slug;
        let code = &it.code;
        // The bridge panel's rig tools key off `rig_slug`; models must clear it
        // so a rig bound earlier cannot leak into the next send.
        let rig_slug = if it.kind == "rig" { slug.as_str() } else { "" };
        let spec = abs_win(root, &format!("{}/{}.json", it.path(), slug));
        let fallbacks = it
            .textures
            .iter()
            .map(|t| format!("r'{}'", abs_win(root, t)))
            .collect::<Vec<_>>()
            .join(", ");

        // Bind `scene.assetlib` only after the FBX importer succeeded: the
        // panel must not advertise a name for an asset that failed to import.
        lines.push(format!(
            "try:\n    with open(r'{fbx}', 'rb') as _fh:\n        _head = _fh.read(18)\n    if not _head.startswith(b'Kaydara FBX Binary'):\n        raise RuntimeError('not a binary FBX (Blender does not support ASCII FBX)')\n    _scale = scene.unit_settings.scale_length\n    if not _scale or _scale <= 0:\n        _scale = 1.0\n    _ret = bpy.ops.import_scene.fbx(filepath=r'{fbx}', global_scale=_scale)\n    if 'FINISHED' not in _ret:\n        raise RuntimeError('FBX importer returned %r' % (_ret,))\n    imported.append(r'{fbx}')\n    try:\n        _alb_texture(r'{spec}', r'{root_w}', [{fallbacks}])\n    except Exception as e:\n        bind_errors.append([r'{code}', 'textures: ' + str(e)])\n    try:\n        p = scene.assetlib\n        p.library_root = r'{root_w}'\n        p.asset_rel = r'{rel}'\n        p.slug = r'{slug}'\n        p.code = r'{code}'\n        p.rig_slug = r'{rig_slug}'\n        bound.append(r'{code}')\n    except Exception as e:\n        bind_errors.append([r'{code}', str(e)])\nexcept Exception as e:\n    errors.append([r'{code}', str(e)])"
        ));
    }
    lines.push(
        "result = {'imported': imported, 'errors': errors, 'count': len(imported), 'bound': bound, 'bind_errors': bind_errors}".to_string(),
    );
    lines.join("\n")
}

/// Reject assets Blender cannot import before the scene is cleared. Blender's
/// FBX importer only reads binary FBX; the ASCII ones otherwise wipe the scene
/// and then fail (the bridge panel used to keep the failed name as well).
pub(crate) fn blender_preflight(items: &[Item], root: &str) -> Result<(), String> {
    for it in items {
        let fbx = abs_win(root, &it.fbx_rel());
        let mut file = std::fs::File::open(&fbx)
            .map_err(|e| format!("{}: cannot open {} ({e})", it.code, fbx))?;
        let mut head = [0u8; 18];
        let n = file
            .read(&mut head)
            .map_err(|e| format!("{}: cannot read {} ({e})", it.code, fbx))?;
        if n < head.len() || &head[..] != b"Kaydara FBX Binary" {
            return Err(format!(
                "{}: not a binary FBX (Blender does not support ASCII FBX; re-export it as binary)",
                it.code
            ));
        }
    }
    Ok(())
}

/// Blender addon address; an empty/blank base falls back to the default port.
fn blender_addr(base: &str) -> String {
    let b = base.trim();
    if b.is_empty() {
        BLENDER_ADDON.to_string()
    } else {
        b.trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    }
}

pub fn blender_health(base: &str) -> Dot {
    if tcp_alive(&blender_addr(base), Duration::from_millis(1500)) {
        dot("ok", "Blender connected — ready to import")
    } else {
        dot(
            "down",
            "Blender is not open (enable the Blender MCP addon, port 9876)",
        )
    }
}

/// `mode`: `"import"` (into the open Blender) | `"open"` (launch a new one).
pub fn blender_send(base: &str, root: &str, mode: &str, items: &[Item]) -> Result<String, String> {
    if items.is_empty() {
        return Err("No assets to import".to_string());
    }
    blender_preflight(items, root)?;
    let code = blender_import_code(items, root);

    if mode == "open" {
        std::process::Command::new(BLENDER_EXE)
            .arg("--python-expr")
            .arg(&code)
            .spawn()
            .map_err(|e| format!("Could not launch Blender: {e}"))?;
        return Ok(format!("{{\"ok\":true,\"opened\":{}}}", items.len()));
    }

    let addr = blender_addr(base);
    if !tcp_alive(&addr, Duration::from_millis(1500)) {
        return Err("Blender is not open (enable the Blender MCP addon, port 9876)".to_string());
    }
    let reply = blender_execute(&addr, &code, SEND_TIMEOUT)?;
    if reply.get("status").and_then(|s| s.as_str()) == Some("ok") {
        Ok(reply.to_string())
    } else {
        Err(reply
            .get("error")
            .or_else(|| reply.get("message"))
            .and_then(|e| e.as_str())
            .unwrap_or("Blender returned an error")
            .to_string())
    }
}

/* -------------------------------- Unity ---------------------------------- */

/// `(host, token)` from the Unity MCP config; falls back to the given base.
fn unity_config(base: &str) -> (String, String) {
    let mut host = if base.trim().is_empty() {
        "http://localhost:26440".to_string()
    } else {
        base.trim_end_matches('/').to_string()
    };
    let mut token = String::new();
    if let Ok(text) = std::fs::read_to_string(UNITY_CONFIG) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(h) = v.get("host").and_then(|s| s.as_str()) {
                if !h.trim().is_empty() {
                    host = h.trim().trim_end_matches('/').to_string();
                }
            }
            if let Some(t) = v.get("token").and_then(|s| s.as_str()) {
                token = t.trim().to_string();
            }
        }
    }
    (host, token)
}

/// Run a C# snippet in the Unity editor and return its textual result. Snippet
/// mode is used instead of a generated class: the class form needs the editor
/// assembly references and can hang on this project.
fn unity_eval(base: &str, code: &str) -> Result<String, String> {
    let (host, token) = unity_config(base);
    let body = json!({
        "csharpCode": code,
        "isMethodBody": true,
    });
    let url = format!("{host}/api/tools/script-execute");
    let (status, text) = http_post(&url, &body, Some(&token), SEND_TIMEOUT)
        .map_err(|e| format!("Unity is not running or MCP is not ready: {e}"))?;
    if status == 404 {
        return Err("Unity is not running or MCP is not ready (404)".to_string());
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| format!("Unity returned non-JSON: {text}"))?;
    if v.get("status").and_then(|s| s.as_str()) != Some("success") {
        return Err(v
            .get("message")
            .or_else(|| v.get("error"))
            .and_then(|m| m.as_str())
            .unwrap_or("Unity call failed")
            .to_string());
    }
    let raw = v
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    let raw = raw.strip_prefix("System.String: ").unwrap_or(raw).trim();
    let unquoted = serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_string());
    Ok(unquoted)
}

pub fn unity_health(base: &str) -> Dot {
    let (host, _) = unity_config(base);
    match http_get(&format!("{host}/api/tools"), HEALTH_TIMEOUT) {
        Ok((200, _)) => dot("ok", "Unity connected — imported list ready"),
        Ok((code, _)) => dot("warn", &format!("Unity MCP answered HTTP {code}")),
        Err(_) => dot("down", "Unity is not running (MCP not listening)"),
    }
}

/// Slugs the Unity ledger already holds (imported into the project).
pub fn unity_imported_slugs(base: &str) -> Result<Vec<String>, String> {
    let code = r#"var ledger = HKLGame.AssetLib.AssetLibraryLedgerStore.Load();
return string.Join(",", ledger.items.ConvertAll(i => i.slug));"#;
    let text = unity_eval(base, code)?;
    Ok(text
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/* ------------------------------- Painter --------------------------------- */

/// Python injected into Painter; reads `params`, assigns `result`.
const PAINTER_IMPORT_CODE: &str = r#"import substance_painter.project as project
import substance_painter.resource as resource
if params["scope"] == "project":
    if not project.is_open():
        raise RuntimeError("No project is open")
    if project.is_busy():
        raise RuntimeError("Painter is busy")
usage = resource.Usage.__members__[params["usage"]]
if params["scope"] == "project":
    item = resource.import_project_resource(params["file_path"], usage, params.get("name"), params.get("group"))
else:
    item = resource.import_session_resource(params["file_path"], usage, params.get("name"), params.get("group"))
identifier = item.identifier()
retrieved = resource.Resource.retrieve(identifier)
result = {"source_path": params["file_path"], "url": identifier.url(), "location": item.location().name,
          "type": item.type().name, "verified": any(c.identifier() == identifier for c in retrieved)}"#;

const PAINTER_OPEN_CODE: &str = r#"import substance_painter.project as project
import substance_painter.textureset as textureset
if project.is_busy():
    raise RuntimeError("Painter is busy")
if project.is_open() and project.needs_saving() and not params.get("force"):
    raise RuntimeError("A project with unsaved changes is open; pass force=true to replace it")
if project.is_open():
    project.close()
project.create(params["mesh"], [], None, None)
result = {"open": True, "name": project.name(), "path": str(project.file_path()) if project.file_path() else None,
          "texture_sets": [(t.name() if callable(t.name) else t.name) for t in textureset.all_texture_sets()]}"#;

/// Run a Python snippet inside Painter via its remote-scripting endpoint.
fn painter_exec(
    remote: &str,
    code: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base = remote.trim_end_matches('/');
    let wrapper = format!(
        "(lambda _ns: (exec(compile(__import__('base64').b64decode('{}').decode('utf-8'), \
         '<app>', 'exec'), _ns, _ns), __import__('json').dumps({{'success': True, 'data': _ns.get('result')}}, \
         ensure_ascii=False, default=str))[1])({{'params': __import__('json').loads(__import__('base64').b64decode('{}').decode('utf-8'))}})",
        b64(code),
        b64(&params.to_string()),
    );
    let body = json!({ "python": b64(&wrapper) });
    let (status, text) = http_post(&format!("{base}/run.json"), &body, None, SEND_TIMEOUT)
        .map_err(|e| format!("Painter is not reachable ({base}): {e}"))?;
    if status != 200 {
        return Err(format!("Painter returned HTTP {status}"));
    }
    let v: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|_| format!("Painter returned invalid JSON: {}", text.trim()))?;
    if let Some(err) = v.get("error") {
        return Err(err.to_string());
    }
    if v.get("success").and_then(|s| s.as_bool()) != Some(true) {
        return Err(format!("Painter returned an invalid envelope: {v}"));
    }
    Ok(v.get("data").cloned().unwrap_or(serde_json::Value::Null))
}

fn painter_alive(remote: &str) -> bool {
    let base = remote.trim_end_matches('/');
    let body = json!({ "js": b64("alg.version.painter") });
    matches!(
        http_post(&format!("{base}/run.json"), &body, None, HEALTH_TIMEOUT),
        Ok((200, _))
    )
}

pub fn painter_health(_bridge: &str, remote: &str) -> Dot {
    if painter_alive(remote) {
        dot("ok", "Substance Painter connected — ready to import")
    } else {
        dot(
            "down",
            "Painter is not reachable (start it with --enable-remote-scripting, port 60041)",
        )
    }
}

/// `mode`: `"import"` (textures) | `"open"` (a project from an FBX).
pub fn painter_send_with_root(
    remote: &str,
    root: &str,
    mode: &str,
    items: &[Item],
) -> Result<String, String> {
    if items.is_empty() {
        return Err("No assets to send".to_string());
    }
    if !painter_alive(remote) {
        return Err(
            "Painter is not reachable (start it with --enable-remote-scripting, port 60041)"
                .to_string(),
        );
    }
    if mode == "open" {
        let it = &items[0];
        let mesh = abs_win(root, &format!("{}/{}.fbx", it.path(), it.slug));
        let data = painter_exec(
            remote,
            PAINTER_OPEN_CODE,
            &json!({ "mesh": mesh, "force": false }),
        )?;
        return Ok(data.to_string());
    }

    let scope = if painter_exec(
        remote,
        "import substance_painter.project as project\nresult = bool(project.is_open())",
        &json!({}),
    )
    .map(|v| v.as_bool().unwrap_or(false))
    .unwrap_or(false)
    {
        "project"
    } else {
        "session"
    };

    let mut imported = 0usize;
    let mut first_error = String::new();
    for it in items {
        for tex in &it.textures {
            let params = json!({
                "file_path": abs_win(root, tex).replace('\\', "/"),
                "usage": "TEXTURE",
                "name": it.code,
                "group": "asset-browser",
                "scope": scope,
            });
            match painter_exec(remote, PAINTER_IMPORT_CODE, &params) {
                Ok(_) => imported += 1,
                Err(e) => {
                    if first_error.is_empty() {
                        first_error = e;
                    }
                }
            }
        }
    }
    if first_error.is_empty() {
        Ok(format!("{{\"imported\":{imported},\"scope\":\"{scope}\"}}"))
    } else {
        Err(first_error)
    }
}

pub fn blender_targets(items: &[Item]) -> Vec<Item> {
    items
        .iter()
        .filter(|it| it.kind() != Kind::Material)
        .cloned()
        .collect()
}

pub fn painter_targets(items: &[Item]) -> Vec<Item> {
    items
        .iter()
        .filter(|it| it.kind() == Kind::Material)
        .cloned()
        .collect()
}
