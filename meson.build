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
  'data/style/_path_bar.scss',
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

# Cargo builds the Rust crate, embedding the GResource bundle via RSIMGV_GRESOURCE.
rsimgv = custom_target('rsimgv',
  input: gresource,
  output: 'rsimgv',
  command: [
    sh, files('build-aux/cargo-build.sh'),
    find_program('cargo').full_path(),
    meson.project_source_root() / 'Cargo.toml',
    meson.project_build_root() / 'target',
    cargo_profile,
    '@INPUT@',
    '@OUTPUT@',
  ],
  build_always_stale: true,
  install: true,
  install_dir: get_option('bindir'),
)

subdir('data')
