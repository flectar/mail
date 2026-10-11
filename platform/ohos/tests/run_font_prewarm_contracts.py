#!/usr/bin/env python3
"""Host-only production/draft FontContext prewarm contracts; no Cargo or device.
Run with explicit existing dependency .rlibs. The actual helper bodies and complete
font builder are freshly extracted from --source, with only target cfg branches
adapted so the OHOS path executes on the host. Constructor gate/counter/panic hooks
are private fixture instrumentation; the font builder is unchanged. Font diagnostics use an explicit callback, never environment mutation.
"""
import argparse, hashlib, json, os, re, subprocess
from pathlib import Path

parser=argparse.ArgumentParser()
parser.add_argument('--source', type=Path, required=True)
parser.add_argument('--main-source', type=Path, required=True)
parser.add_argument('--deps-dir', type=Path, required=True)
parser.add_argument('--parley-rlib', type=Path, required=True)
parser.add_argument('--tokio-rlib', type=Path, required=True)
parser.add_argument('--output-dir', type=Path, required=True)
parser.add_argument('--cjk-font', type=Path, required=True, help='Existing CJK font covering the Chinese and Traditional Chinese fixture characters')
parser.add_argument('--repository',type=Path,default=Path(__file__).resolve().parents[3])
parser.add_argument('--ohos-parley-rlib',type=Path)
parser.add_argument('--ohos-deps-dir',type=Path)
parser.add_argument('--host-proc-macro-dir',type=Path)
args=parser.parse_args()
args.repository=args.repository.resolve()
args.cjk_font=args.cjk_font.resolve()
for asset in [args.cjk_font,
              args.repository / 'resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf',
              args.repository / 'crates/blitz-dom/assets/moz-bullet-font.otf',
              args.repository / 'resources/fonts/noto-emoji/NotoEmoji[wght].ttf']:
    if not asset.is_file():parser.error('required font fixture does not exist: '+str(asset))
# Compile-time environment inputs preserve arbitrary path characters without
# rewriting the Rust fixture or assuming one checkout or system font location.
fixture_env=dict(os.environ)
fixture_env['FEIHONG_CONTRACT_REPOSITORY']=str(args.repository)
fixture_env['FEIHONG_CONTRACT_CJK_FONT']=str(args.cjk_font)
source=args.source.read_text()
output=args.output_dir
output.mkdir(mode=0o700,parents=True,exist_ok=True)

def function(name, text=None):
    text=source if text is None else text
    match=re.search(r'(?:pub(?:\(crate\))? )?fn '+re.escape(name)+r'\(',text)
    if not match: raise RuntimeError('actual function not found: '+name)
    start=match.start(); pos=text.index('{',start);depth=1;end=pos+1
    while depth:
        if text[end]=='{':depth+=1
        elif text[end]=='}':depth-=1
        end+=1
    return text[start:end]

statement=re.search(r'static PREWARMED_EMAIL_FONT_CTX:\s*std::sync::OnceLock<parley::FontContext>\s*=\s*std::sync::OnceLock::new\(\);',source)
if not statement: raise RuntimeError('actual ready publication static not found')
logger_statement=re.search(r'static EMAIL_FONT_DIAGNOSTICS:\s*std::sync::OnceLock<fn\(&str\)>\s*=\s*std::sync::OnceLock::new\(\);',source)
if not logger_statement:raise RuntimeError('actual diagnostic static not found')
helpers=statement.group()+'\n#[cfg(debug_assertions)]\n'+logger_statement.group()+'\n'+ '\n'.join(function(n) for n in ['configure_email_font_diagnostics','email_font_diagnostics','prewarm_email_font_ctx','initial_email_font_ctx','build_email_font_ctx'])+'\n'
# Do not mirror or change helper behavior. Remove target cfg only for host execution.
helpers=re.sub(r'\s*#\[cfg\(target_env = "ohos"\)\]\n','\n',helpers)
helpers=helpers.replace('    #[cfg(not(target_env = "ohos"))]\n    create_email_font_ctx()\n','')
if 'target_env' in helpers:raise RuntimeError('unexpected target branch in extracted helper')
builder=function('create_email_font_ctx_with_system_fonts')
builder=re.sub(r'    #\[cfg\(target_env = "ohos"\)\]\n','',builder)
harness=Path(__file__).with_name('font_prewarm_contracts.rs').read_text()
main_text=args.main_source.read_text()
forwarder=function('configure_platform_font_diagnostics',main_text)
for filename,text in [('actual_prewarm_functions.rs',helpers),('production_builder_read.rs',builder),('actual_main_font_diagnostics.rs','use crate as renderer;\n'+forwarder+'\n'),('actual_prewarm_contracts.rs',harness)]:
    path=output/filename;path.write_text(text);path.chmod(0o600)
# The contract has a fixed observer gate; the host --system_fonts=false behavior
# is explicitly kept because native host font discovery differs from OHOS.
command=['rustc','--edition=2024',str(output/'actual_prewarm_contracts.rs'),'-L','dependency='+str(args.deps_dir),'--extern','parley='+str(args.parley_rlib),'--extern','tokio='+str(args.tokio_rlib),'-o',str(output/'actual_prewarm_contracts')]
compiled=subprocess.run(command,env=fixture_env,text=True,capture_output=True,timeout=45)
(output/'compile.log').write_text(compiled.stdout+compiled.stderr);(output/'compile.log').chmod(0o600)
if compiled.returncode:raise RuntimeError('host rustc contract build failed: see compile.log')
results=[]
for mode in ['ready','pending','panic']:
    env=dict(os.environ);env.pop('FLECTAR_RENDER_TIMINGS',None)
    result=subprocess.run([str(output/'actual_prewarm_contracts'),mode],env=env,text=True,capture_output=True,timeout=8)
    log=output/(mode+'.log');log.write_text(result.stdout+result.stderr);log.chmod(0o600)
    if result.returncode or 'FeihongFontInit' in result.stderr:raise RuntimeError(mode+' failed: see '+str(log))
    results.append({'mode':mode,'status':'PASS','diagnostics_default_off':True,'log':str(log)})
env=dict(os.environ);env.pop('FLECTAR_RENDER_TIMINGS',None)
result=subprocess.run([str(output/'actual_prewarm_contracts'),'ready-diagnostics'],env=env,text=True,capture_output=True,timeout=8)
log=output/'ready-diagnostics.log';log.write_text(result.stdout+result.stderr);log.chmod(0o600)
if result.returncode or 'background=1 success=1 published=1 bootstrap_ms=' not in result.stderr or 'background=0 ready=1 bootstrap_ms=' not in result.stderr:raise RuntimeError('explicit diagnostics failed')
results.append({'mode':'ready-diagnostics','status':'PASS','diagnostics_explicitly_enabled':True,'log':str(log)})
# The pre-existing broad renderer timing environment must not implicitly enable this logger.
env=dict(os.environ);env['FLECTAR_RENDER_TIMINGS']='1'
result=subprocess.run([str(output/'actual_prewarm_contracts'),'ready'],env=env,text=True,capture_output=True,timeout=8)
log=output/'existing-env-default-off.log';log.write_text(result.stdout+result.stderr);log.chmod(0o600)
if result.returncode or 'FeihongFontInit' in result.stderr:raise RuntimeError('font logger implicitly enabled by broad timing env')
results.append({'mode':'existing-env-default-off','status':'PASS','log':str(log)})
# Release-style cfg must ignore the explicit diagnostic callback entirely.
release_command=command[:-2]+['-C','debug-assertions=no','-o',str(output/'actual_prewarm_contracts_release_cfg')]
compiled_release=subprocess.run(release_command,env=fixture_env,text=True,capture_output=True,timeout=45)
(output/'compile-release-cfg.log').write_text(compiled_release.stdout+compiled_release.stderr);(output/'compile-release-cfg.log').chmod(0o600)
if compiled_release.returncode:raise RuntimeError('host release-cfg compile failed')
result=subprocess.run([str(output/'actual_prewarm_contracts_release_cfg'),'ready-diagnostics'],text=True,capture_output=True,timeout=8)
log=output/'release-cfg-diagnostics-off.log';log.write_text(result.stdout+result.stderr);log.chmod(0o600)
if result.returncode or 'FeihongFontInit' in result.stderr:raise RuntimeError('release-cfg font diagnostics were enabled')
results.append({'mode':'release-cfg-diagnostics-off','status':'PASS','log':str(log)})
negative_controls=[]
# Mutation controls exercise this same actual-source harness rather than a
# second implementation of the intended cache behavior.
for name,mutated_helpers,mutated_builder,expected in [
    ('shared-owner-font-cache',helpers.replace('static FONTS: parley::FontContext = initial_email_font_ctx();','static FONTS: parley::FontContext = { let mut font_ctx = initial_email_font_ctx(); font_ctx.collection.make_shared(); font_ctx };'),builder,'assertion_failure'),
    ('published-panic-default',helpers.replace('let result = std::panic::catch_unwind(create_email_font_ctx);','let result = std::panic::catch_unwind(create_email_font_ctx).map_err(|_| ()).or_else(|()| Ok::<parley::FontContext, ()>(parley::FontContext::default()));'),builder,'assertion_failure'),
    ('blocking-owner-prewarm',helpers.replace('runtime.spawn_blocking(|| {','let initialization = runtime.spawn_blocking(|| {').replace('    });\n}', '    });\n    runtime.block_on(initialization).expect("private negative control blocking join");\n}'),builder,'timeout'),
]:
    directory=output/('negative-'+name);directory.mkdir(mode=0o700,exist_ok=True)
    if name in ['shared-owner-font-cache','published-panic-default'] and mutated_helpers==helpers:raise RuntimeError('negative mutation missed: '+name)
    if name=='blocking-owner-prewarm' and mutated_helpers==helpers:raise RuntimeError('blocking mutation missed')
    for filename,text in [('actual_prewarm_functions.rs',mutated_helpers),('production_builder_read.rs',mutated_builder),('actual_main_font_diagnostics.rs','use crate as renderer;\n'+forwarder+'\n'),('actual_prewarm_contracts.rs',harness)]:
        path=directory/filename;path.write_text(text);path.chmod(0o600)
    binary=directory/'contracts'
    negative_command=['rustc','--edition=2024',str(directory/'actual_prewarm_contracts.rs'),'-L','dependency='+str(args.deps_dir),'--extern','parley='+str(args.parley_rlib),'--extern','tokio='+str(args.tokio_rlib),'-o',str(binary)]
    built=subprocess.run(negative_command,env=fixture_env,text=True,capture_output=True,timeout=45)
    log=directory/'compile.log';log.write_text(built.stdout+built.stderr);log.chmod(0o600)
    if built.returncode:raise RuntimeError('negative control failed to compile: '+name)
    timed_out=False
    try:
        result=subprocess.run([str(binary),'panic' if name=='published-panic-default' else ('ready' if name=='shared-owner-font-cache' else 'pending')],text=True,capture_output=True,timeout=5)
        logs=result.stdout+result.stderr;exit_code=result.returncode
    except subprocess.TimeoutExpired as error:
        timed_out=True;exit_code=None
        def decode(value):return value.decode() if isinstance(value,bytes) else (value or '')
        logs=decode(error.stdout)+decode(error.stderr)+'\nEXPECTED: owner blocked while background gate remained closed\n'
    log=directory/'run.log';log.write_text(logs);log.chmod(0o600)
    if expected=='assertion_failure' and (timed_out or exit_code==0 or 'panicked at' not in logs):raise RuntimeError('assertion control unexpectedly passed: '+name)
    if expected=='timeout' and not timed_out:raise RuntimeError('blocking owner control did not detect blocking')
    negative_controls.append({'mutation':name,'status':'EXPECTED_FAILURE','exit_code':exit_code,'timed_out':timed_out,'log':str(log),'actual_mutated_helper_sha256':hashlib.sha256(mutated_helpers.encode()).hexdigest(),'actual_mutated_builder_sha256':hashlib.sha256(mutated_builder.encode()).hexdigest()})
ohOS_metadata=[]
if any([args.ohos_parley_rlib,args.ohos_deps_dir,args.host_proc_macro_dir]):
    if not all([args.ohos_parley_rlib,args.ohos_deps_dir,args.host_proc_macro_dir]):raise RuntimeError('provide all OHOS metadata arguments')
    # This compiles the actual extracted functions including Tokio spawn_blocking and
    # OnceLock<FontContext>, against the existing actual OHOS library metadata.
    for debug_cfg in [True,False]:
        cmd=['rustc','--edition=2024','--crate-type=lib','--target','aarch64-unknown-linux-ohos','--emit=metadata',str(output/'actual_prewarm_contracts.rs'),'-L','dependency='+str(args.ohos_deps_dir),'-L','dependency='+str(args.host_proc_macro_dir),'--extern','parley='+str(args.ohos_parley_rlib),'--extern','tokio='+str(args.ohos_deps_dir / next(p.name for p in sorted(args.ohos_deps_dir.glob('libtokio-*.rlib'),key=lambda p:p.stat().st_mtime,reverse=True))),'-C','debug-assertions='+('yes' if debug_cfg else 'no'),'-o',str(output/('functions-ohos-'+str(debug_cfg)+'.rmeta'))]
        checked=subprocess.run(cmd,env=fixture_env,text=True,capture_output=True,timeout=45)
        log=output/('metadata-ohos-'+str(debug_cfg)+'.log');log.write_text(checked.stdout+checked.stderr);log.chmod(0o600)
        if checked.returncode:raise RuntimeError('actual OHOS function metadata failed: '+str(log))
        ohOS_metadata.append({'debug_assertions':debug_cfg,'status':'PASS','kind':'target metadata only, not runtime','command':cmd,'log':str(log)})
main_validation=None
if args.main_source:
    main=args.main_source.read_text()
    emoji=main.index('configure_emoji_font_fallback()?;',main.index('pub fn run('))
    spawn=main.index('renderer::prewarm_email_font_ctx(runtime.handle());',emoji)
    window=main.index('AppWindow::new',emoji)
    if not emoji<spawn<window:raise RuntimeError('prewarm startup ordering failed')
    if '#[cfg(target_env = "ohos")]' not in main[max(emoji,spawn-100):spawn]:raise RuntimeError('main prewarm is not OHOS-gated')
    main_validation={'sha256':hashlib.sha256(main.encode()).hexdigest(),'emoji_registration_then_prewarm_then_window':'PASS'}
report={'host_only':True,'production_or_draft_source':str(args.source),'source_sha256':hashlib.sha256(source.encode()).hexdigest(),'main':main_validation,'runner_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'harness_sha256':hashlib.sha256(harness.encode()).hexdigest(),'fixture_inputs':{'repository':str(args.repository),'cjk_font':str(args.cjk_font),'cjk_font_sha256':hashlib.sha256(args.cjk_font.read_bytes()).hexdigest(),'transport':'rustc compile-time environment; no source path interpolation'},'parley_rlib':str(args.parley_rlib),'tokio_rlib':str(args.tokio_rlib),'compile_command':command,'cases':results,'negative_controls':negative_controls,'ohos_function_metadata':ohOS_metadata,'private_adaptations':['OHOS cfg branch enabled on host','constructor hooks for gate/counter/panic','complete actual font builder uses system_fonts=false with actual host CJK + production emoji; native font discovery differs'],'limits':['no HAP/device/FPS proof','ready miss preserves original synchronous fallback cost','exact template-clone order preserved; independent multi-family TTC rebuild ordering in Fontique is nondeterministic']}
path=output/'validation.json';path.write_text(json.dumps(report,indent=2)+'\n');path.chmod(0o600)
print('PASS: actual extracted FontContext prewarm ready/pending/panic/isolation/default-off/opt-in contracts; '+str(path))
