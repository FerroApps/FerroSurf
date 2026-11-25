//! Generation of the per-extension injected JavaScript program.

use crate::extension::Extension;
use crate::match_pattern::MatchPattern;

fn json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn regex_list(ext: &Extension, patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .filter_map(|p| match MatchPattern::parse(p) {
            Ok(mp) => Some(format!("new RegExp({})", json(&mp.to_js_regex()))),
            Err(e) => {
                log::warn!("extension {}: bad match pattern {p:?}: {e}", ext.id);
                None
            }
        })
        .collect()
}

fn read_text(ext: &Extension, rel: &str) -> Option<String> {
    let p = ext.resolve(rel)?;
    match std::fs::read(&p) {
        Ok(b) => Some(
            String::from_utf8_lossy(&b)
                .trim_start_matches('\u{feff}')
                .to_owned(),
        ),
        Err(e) => {
            log::warn!("extension {}: cannot read {rel}: {e}", ext.id);
            None
        }
    }
}

struct Entry {
    inc: Vec<String>,
    exc: Vec<String>,
    run_at: &'static str,
    all_frames: bool,
    css: String,
    files: Vec<String>,
}

pub(crate) fn generate(ext: &Extension) -> Option<String> {
    let m = &ext.manifest;
    let err_label = json(&format!("[ext {}]", m.name));
    let mut entries: Vec<Entry> = Vec::new();

    for cs in &m.content_scripts {
        let inc = regex_list(ext, &cs.matches);
        if inc.is_empty() {
            continue;
        }
        let mut css = String::new();
        for f in &cs.css {
            match read_text(ext, f) {
                Some(t) => {
                    css.push_str(&t);
                    css.push('\n');
                }
                None => log::warn!("extension {}: missing css file {f}", ext.id),
            }
        }
        let mut files = Vec::new();
        for f in &cs.js {
            match read_text(ext, f) {
                Some(src) => files.push(format!(
                    "function(){{try{{(function(chrome,browser){{\n{src}\n}}).call(window,chrome,browser);}}catch(e){{console.error({err_label},e);}}}}"
                )),
                None => log::warn!("extension {}: missing js file {f}", ext.id),
            }
        }
        if files.is_empty() && css.is_empty() {
            continue;
        }
        entries.push(Entry {
            inc,
            exc: regex_list(ext, &cs.exclude_matches),
            run_at: cs.run_at.as_str(),
            all_frames: cs.all_frames,
            css,
            files,
        });
    }

    if let Some(f) = &m.ferrosurf {
        for us in &f.user_styles {
            let inc = regex_list(ext, &us.matches);
            if inc.is_empty() {
                continue;
            }
            let css = if !us.css.contains(['{', '}']) && ext.resolve(&us.css).is_some() {
                read_text(ext, &us.css).unwrap_or_default()
            } else {
                us.css.clone()
            };
            if css.trim().is_empty() {
                continue;
            }
            entries.push(Entry {
                inc,
                exc: Vec::new(),
                run_at: "document_start",
                all_frames: false,
                css,
                files: Vec::new(),
            });
        }
    }

    if entries.is_empty() {
        return None;
    }

    let mut manifest = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
    if let Some(o) = manifest.as_object_mut() {
        o.remove("ferrosurf");
    }
    let messages = crate::manifest::load_messages(&ext.root, m.default_locale.as_deref());

    let mut js = String::with_capacity(4096);
    js.push_str("(function(){\n");
    js.push_str(&format!("var EXT_ID={};\n", json(&ext.id)));
    js.push_str(&format!("var EXT_NAME={};\n", json(&m.name)));
    js.push_str(&format!("var MANIFEST={};\n", json(&manifest)));
    js.push_str(&format!("var MESSAGES={};\n", json(&messages)));
    js.push_str(SHIM);
    js.push_str("var ENTRIES=[\n");
    for e in &entries {
        js.push_str(&format!(
            "{{inc:[{}],exc:[{}],runAt:{},all:{},css:{},files:[{}]}},\n",
            e.inc.join(","),
            e.exc.join(","),
            json(e.run_at),
            e.all_frames,
            json(&e.css),
            e.files.join(",\n"),
        ));
    }
    js.push_str("];\n");
    js.push_str(RUNNER);
    js.push_str("})();\n");
    Some(js)
}

const SHIM: &str = r#"var PREFIX="__ferrosurf_ext_"+EXT_ID+"__";
var TOP=false;try{TOP=window.top===window;}catch(e){}
var HREF=String(location.href);
function lsGet(k){try{return window.localStorage.getItem(k);}catch(e){return null;}}
function lsSet(k,v){try{window.localStorage.setItem(k,v);}catch(e){console.error("[ext "+EXT_NAME+"]",e);}}
function lsDel(k){try{window.localStorage.removeItem(k);}catch(e){}}
function lsKeys(){var r=[];try{var ls=window.localStorage;for(var i=0;i<ls.length;i++){var k=ls.key(i);if(k&&k.indexOf(PREFIX)===0)r.push(k.slice(PREFIX.length));}}catch(e){}return r;}
function parse(s){try{return JSON.parse(s);}catch(e){return undefined;}}
function clone(o){return o===undefined?undefined:JSON.parse(JSON.stringify(o));}
function respond(fn,cb){
  var p=new Promise(function(res,rej){try{res(fn());}catch(e){rej(e);}});
  if(typeof cb==="function"){p.then(function(v){cb(v);},function(e){console.error("[ext "+EXT_NAME+"]",e);cb();});return undefined;}
  return p;
}
function keyList(keys){return typeof keys==="string"?[keys]:Array.isArray(keys)?keys:Object.keys(keys||{});}
var area={
  get:function(keys,cb){
    if(typeof keys==="function"){cb=keys;keys=null;}
    return respond(function(){
      var out={};
      if(keys===null||keys===undefined){lsKeys().forEach(function(k){var v=parse(lsGet(PREFIX+k));if(v!==undefined)out[k]=v;});return out;}
      var defs=(typeof keys==="object"&&!Array.isArray(keys))?keys:{};
      keyList(keys).forEach(function(k){
        var raw=lsGet(PREFIX+k);var v=raw===null?undefined:parse(raw);
        if(v!==undefined)out[k]=v;else if(Object.prototype.hasOwnProperty.call(defs,k))out[k]=clone(defs[k]);
      });
      return out;
    },cb);
  },
  set:function(items,cb){return respond(function(){Object.keys(items||{}).forEach(function(k){if(items[k]!==undefined)lsSet(PREFIX+k,JSON.stringify(items[k]));});},cb);},
  remove:function(keys,cb){return respond(function(){keyList(keys).forEach(function(k){lsDel(PREFIX+k);});},cb);},
  clear:function(cb){return respond(function(){lsKeys().forEach(function(k){lsDel(PREFIX+k);});},cb);}
};
function getURL(p){return "ferro-extension://"+EXT_ID+"/"+String(p||"").replace(/^\/+/,"");}
function getMessage(name,subs){
  var m=MESSAGES[String(name).toLowerCase()];if(m===undefined)return "";
  if(subs===undefined)subs=[];else if(!Array.isArray(subs))subs=[subs];
  return m.replace(/\$(\d)/g,function(_,d){var v=subs[+d-1];return v===undefined?"":String(v);});
}
function evt(){return{addListener:function(){},removeListener:function(){},hasListener:function(){return false;}};}
var chrome={
  runtime:{id:EXT_ID,lastError:null,getManifest:function(){return clone(MANIFEST);},getURL:getURL,
    sendMessage:function(){var cb=arguments[arguments.length-1];if(typeof cb==="function"){cb();return undefined;}return Promise.resolve();},
    connect:function(){return{postMessage:function(){},disconnect:function(){},onMessage:evt(),onDisconnect:evt()};},
    onMessage:evt(),onInstalled:evt()},
  extension:{getURL:getURL},
  storage:{local:area,sync:area,onChanged:evt()},
  i18n:{getMessage:getMessage,getUILanguage:function(){return navigator.language||"en";}}
};
var browser=chrome;
"#;

const RUNNER: &str = r#"function testAny(list,href){for(var i=0;i<list.length;i++){if(list[i].test(href))return true;}return false;}
function injectCss(text){
  function add(){var st=document.createElement("style");st.setAttribute("data-ferrosurf-ext",EXT_ID);st.textContent=text;(document.head||document.documentElement).appendChild(st);}
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
  if(!testAny(s.inc,HREF)||testAny(s.exc,HREF))return;
  if(s.css)injectCss(s.css);
  if(s.files.length)schedule(s.runAt,function(){s.files.forEach(function(f){f();});});
});
"#;
