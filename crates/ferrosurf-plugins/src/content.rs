//! The injected JavaScript program of a plugin's static `[[content]]`
//! entries: match-pattern guards, CSS injection and `run_at` scheduling,
//! like the WebExtension user scripts in `ferrosurf-extensions`.
//!
//! Plugin scripts get one extra binding, `ferrosurf`, with
//! `ferrosurf.call(payload)`: a promise for the plugin's `page_message`
//! reply (it uses the app's `window.__ferrosurf.call` bridge).

use ferrosurf_extensions::MatchPattern;

use crate::manifest::Manifest;

fn json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn regexes(id: &str, patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .filter_map(|p| match MatchPattern::parse(p) {
            Ok(mp) => Some(format!("new RegExp({})", json(&mp.to_js_regex()))),
            Err(e) => {
                log::warn!("plugin {id}: bad match pattern {p:?}: {e}");
                None
            }
        })
        .collect()
}

/// The wrapper program for `m`, given the text of each entry's `(js, css)`
/// (same order as `m.content`). `None` when nothing would ever run.
pub fn wrapper(m: &Manifest, assets: &[(Option<String>, Option<String>)]) -> Option<String> {
    let label = json(&format!("[plugin {}]", m.name));
    let mut entries = Vec::new();
    for (spec, (js, css)) in m.content.iter().zip(assets) {
        let inc = regexes(&m.id, &spec.matches);
        if inc.is_empty() || (js.is_none() && css.is_none()) {
            continue;
        }
        let file = js
            .as_ref()
            .map(|src| {
                format!(
                    "function(){{try{{(function(ferrosurf){{\n{src}\n}}).call(window,FERRO);}}catch(e){{console.error({label},e);}}}}"
                )
            })
            .unwrap_or_default();
        entries.push(format!(
            "{{inc:[{}],runAt:{},all:{},css:{},file:{}}}",
            inc.join(","),
            json(spec.run_at.as_str()),
            spec.all_frames,
            json(css.as_deref().unwrap_or("")),
            if file.is_empty() { "null".into() } else { file },
        ));
    }
    if entries.is_empty() {
        return None;
    }
    let mut js = String::with_capacity(2048);
    js.push_str("(function(){\n");
    js.push_str(&format!("var PLUGIN_ID={};\n", json(&m.id)));
    js.push_str(PRELUDE);
    js.push_str("var ENTRIES=[\n");
    js.push_str(&entries.join(",\n"));
    js.push_str("\n];\n");
    js.push_str(RUNNER);
    js.push_str("})();\n");
    Some(js)
}

const PRELUDE: &str = r#"var FERRO=Object.freeze({pluginId:PLUGIN_ID,call:function(payload){
  var b=window.__ferrosurf;
  if(!b||typeof b.call!=="function")return Promise.reject(new Error("ferrosurf bridge unavailable"));
  return b.call("plugin:"+PLUGIN_ID,payload);
}});
var TOP=false;try{TOP=window.top===window;}catch(e){}
var HREF=String(location.href);
"#;

const RUNNER: &str = r#"function testAny(list,href){for(var i=0;i<list.length;i++){if(list[i].test(href))return true;}return false;}
function injectCss(text){
  function add(){var st=document.createElement("style");st.setAttribute("data-ferrosurf-plugin",PLUGIN_ID);st.textContent=text;(document.head||document.documentElement).appendChild(st);}
  if(document.documentElement){add();}
  else{var mo=new MutationObserver(function(){if(document.documentElement){mo.disconnect();add();}});mo.observe(document,{childList:true});}
}
function schedule(runAt,fn){
  if(runAt==="document_start"){fn();}
  else if(runAt==="document_end"){
    if(document.readyState!=="loading")fn();else document.addEventListener("DOMContentLoaded",fn,{once:true});
  }else{
    var go=function(){if(typeof window.requestIdleCallback==="function")window.requestIdleCallback(fn,{timeout:2000});else setTimeout(fn,0);};
    if(document.readyState==="complete")go();else window.addEventListener("load",go,{once:true});
  }
}
ENTRIES.forEach(function(s){
  if(!s.all&&!TOP)return;
  if(!testAny(s.inc,HREF))return;
  if(s.css)injectCss(s.css);
  if(s.file)schedule(s.runAt,s.file);
});
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_guarded_program() {
        let m = Manifest::parse(
            r#"
id = "c"
name = "C"
version = "1"
api_version = 1
permissions = ["page_scripting"]
[[content]]
matches = ["https://*.example.com/*"]
js = "a.js"
css = "a.css"
run_at = "document_start"
"#,
        )
        .unwrap();
        let js = wrapper(&m, &[(Some("console.log(1)".into()), Some("a{}".into()))]).unwrap();
        assert!(js.contains("var PLUGIN_ID=\"c\""));
        assert!(js.contains("console.log(1)"));
        assert!(js.contains("new RegExp("));
        assert!(js.contains("\"document_start\""));
        assert!(wrapper(&m, &[(None, None)]).is_none());
    }
}
