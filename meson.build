project('rsimgv',
  version: '0.1.0',
  license: 'GPL-3.0-or-later',
  meson_version: '>= 1.2.0',
)

gnome = import('gnome')

sassc = find_program('sassc')
sh = find_program('sh')

gtk_dep = dependency('gtk4', version: '>= 4.22')
adw_dep = dependency('libadwaita-1', version: '>= 1.9')
# Videos can be played by libmpv, which the crate links against.
mpv_dep = dependency('mpv', version: '>= 2.0')
# Pictures are resampled by OpenCV, which the opencv crate links against and generates its bindings for with clang.
opencv_dep = dependency('opencv4', 'opencv5', version: '>= 4.5')

# Python plugins embed CPython through PyO3 and build widgets through PyGObject. Both are optional; a build
# without them still runs every non-plugin feature and reports Python plugins as unsupported.
python_option = get_option('python')
python = find_program('python3', required: false)
python_dep = dependency('python3-embed', version: '>= 3.10', required: false)
pygobject_dep = dependency('pygobject-3.0', version: '>= 3.50', required: false)
python_enabled = python_option.enabled() or (python_option.auto() and python.found() and python_dep.found() and pygobject_dep.found())
if python_option.enabled() and not (python.found() and python_dep.found() and pygobject_dep.found())
  error('Python plugin support was enabled, but python3-embed or pygobject-3.0 was not found')
endif
cargo_features = python_enabled ? '--features=python' : '--no-default-features'

if get_option('buildtype') == 'debug'
  cargo_profile = 'debug'
else
  cargo_profile = 'release'
endif

# Compile the SASS entry point to CSS; partials are resolved next to it. The partials are listed so
# that editing one rebuilds the stylesheet.
style_partials = files(
  'data/style/_browse_view.scss',
  'data/style/_content_pane.scss',
  'data/style/_file_tree.scss',
  'data/style/_media_viewer.scss',
  'data/style/_messages.scss',
  'data/style/_path_bar.scss',
  'data/style/_playlist.scss',
  'data/style/_resizable_split_view.scss',
  'data/style/_variables.scss',
)
style_css = custom_target('style',
  input: 'data/style/main.scss',
  output: 'style.css',
  depend_files: style_partials,
  command: [sassc, '--style=expanded', '@INPUT@', '@OUTPUT@'],
)

# Bundle templates, stylesheet and icons into one GResource file.
gresource = gnome.compile_resources('rsimgv',
  'data/rsimgv.gresource.xml',
  gresource_bundle: true,
  source_dir: ['data', meson.current_build_dir()],
  dependencies: [style_css],
)

# SVP frame interpolation uses open-svpflow's VapourSynth plugins, built from its source in third_party/. Their
# libraries are written into the build directory, where Cargo embeds them.
svpflow_plugins = custom_target('svpflow',
  output: ['libsvpflow1_vs.so', 'libsvpflow2_vs.so'],
  command: [
    sh, files('build-aux/svpflow-build.sh'),
    find_program('cargo').full_path(),
    meson.project_source_root() / 'third_party' / 'open-svpflow',
    meson.project_build_root() / 'svpflow-target',
    '@OUTDIR@',
  ],
)

# Cargo builds the Rust crate, embedding the GResource bundle via RSIMGV_GRESOURCE and the plugins via
# RSIMGV_SVP_PLUGINS.
rsimgv = custom_target('rsimgv',
  input: gresource,
  output: 'rsimgv',
  depends: svpflow_plugins,
  command: [
    sh, files('build-aux/cargo-build.sh'),
    find_program('cargo').full_path(),
    meson.project_source_root() / 'Cargo.toml',
    meson.project_build_root() / 'target',
    cargo_profile,
    '@INPUT@',
    meson.current_build_dir(),
    '@OUTPUT@',
  ],
  env: python_enabled
    ? {'PYO3_PYTHON': python.full_path(), 'RSIMGV_CARGO_FEATURES': cargo_features}
    : {'RSIMGV_CARGO_FEATURES': cargo_features},
  build_always_stale: true,
  install: true,
  install_dir: get_option('bindir'),
)

subdir('data')
