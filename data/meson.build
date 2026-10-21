gnome = import('gnome')

install_data(
  'au.topher.rimgv.gschema.xml',
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas',
)

# Compiles the schema for running from the build directory (see `meson devenv`).
gnome.compile_schemas(build_by_default: true)

# Recompiles the installed schemas into gschemas.compiled, which GSettings reads at runtime.
gnome.post_install(glib_compile_schemas: true)

# Application icons, installed into the system icon theme.
install_subdir('icons/hicolor', install_dir: get_option('datadir') / 'icons')
