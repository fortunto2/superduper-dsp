# Plugin manifest — single source of truth for clap-wrapper VST3/AU
# generation. Each entry lists the Rust crate name, the wrapper output
# name (must exactly match the existing `.clap` bundle filename — minus
# the .clap suffix — so the wrapper can find and dynamically load it),
# the CLAP identifier, the bundle identifier prefix, the AUv2
# instrument-type code (`aumu` for instruments, `aufx` for effects),
# and a stable AUv2 subtype code so the host's AU caches don't churn
# across builds.
#
# Fields are separated by `|` so CMake doesn't flatten the list — CMake
# uses `;` as its internal separator, so each row would otherwise
# explode into 6 stand-alone foreach iterations.
#
# IMPORTANT: the `name` field becomes both the VST3/AU bundle filename
# AND the basename clap-wrapper searches for in the system CLAP path.
# Don't add spaces — our Rust bundles are camel-case (SuperDuperReverb,
# SuperDuperWave, …) so the wrappers have to match.

set(SDSP_WRAPPER_PLUGINS
    # crate|wrapper output name|clap id|bundle id prefix|aux type|sub code
    "superduper-reverb|SuperDuperReverb|co.superduperai.reverb|co.superduperai.wrappers.reverb|aufx|sdRv"
    "superduper-supermass|SuperDuperSupermass|co.superduperai.supermass|co.superduperai.wrappers.supermass|aufx|sdSm"
    "superduper-spectrum|SuperDuperSpectrum|co.superduperai.spectrum|co.superduperai.wrappers.spectrum|aufx|sdSp"
    "superduper-saturator|SuperDuperSaturator|co.superduperai.saturator|co.superduperai.wrappers.saturator|aufx|sdSa"
    "superduper-delay|SuperDuperDelay|co.superduperai.delay|co.superduperai.wrappers.delay|aufx|sdDl"
    "superduper-compressor|SuperDuperCompressor|co.superduperai.compressor|co.superduperai.wrappers.compressor|aufx|sdCm"
    "superduper-eq|SuperDuperEq|co.superduperai.eq|co.superduperai.wrappers.eq|aufx|sdEq"
    "superduper-limiter|SuperDuperLimiter|co.superduperai.limiter|co.superduperai.wrappers.limiter|aufx|sdLm"
    "superduper-vocal|SuperDuperVocal|co.superduperai.vocal|co.superduperai.wrappers.vocal|aufx|sdVc"
    "superduper-ambient|SuperDuperAmbient|co.superduperai.ambient|co.superduperai.wrappers.ambient|aumu|sdAm"
    "superduper-pad|SuperDuperPad|co.superduperai.pad|co.superduperai.wrappers.pad|aumu|sdPd"
    "superduper-wave|SuperDuperWave|co.superduperai.wave|co.superduperai.wrappers.wave|aumu|sdWv"
    "superduper-kubyz|SuperDuperKubyz|co.superduperai.kubyz|co.superduperai.wrappers.kubyz|aumu|sdKb"
    "superduper-chorus|SuperDuperChorus|co.superduperai.chorus|co.superduperai.wrappers.chorus|aufx|sdCh"
    "superduper-drum|SuperDuperDrum|co.superduperai.drum|co.superduperai.wrappers.drum|aumu|sdDr"
    "superduper-sampler|SuperDuperSampler|co.superduperai.sampler|co.superduperai.wrappers.sampler|aumu|sdSl"
    "superduper-looper|SuperDuperLooper|co.superduperai.looper|co.superduperai.wrappers.looper|aufx|sdLp"
    "superduper-filter|SuperDuperFilter|co.superduperai.filter|co.superduperai.wrappers.filter|aufx|sdFl"
    "superduper-midside|SuperDuperMidSide|co.superduperai.midside|co.superduperai.wrappers.midside|aufx|sdMs"
    "superduper-lineq|SuperDuperLineq|co.superduperai.lineq|co.superduperai.wrappers.lineq|aufx|sdLe"
    "superduper-soothe|SuperDuperSoothe|co.superduperai.soothe|co.superduperai.wrappers.soothe|aufx|sdSo"
    "superduper-nam|SuperDuperNam|co.superduperai.nam|co.superduperai.wrappers.nam|aufx|sdNm"
)
