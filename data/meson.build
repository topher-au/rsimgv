gnome = import('gnome')

install_data(
  'au.topher.rimgv.gschema.xml',
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas',
)

# Compiles the schema for running from the build directory (see `meson devenv`).
gnome.compile_schemas(build_by_default: true)

# Recompiles the installed schemas into gschemas.compiled, which GSettings reads at runtime.
gnome.post_install(glib_compile_schemas: true)

# The launcher entry, so the app appears in the applications menu. Its name matches the application ID,
# which is how GTK links a running window to it.
install_data(
  'au.topher.rimgv.desktop',
  install_dir: get_option('datadir') / 'applications',
)

# Application icons, installed into the system icon theme.
install_subdir('icons/hicolor', install_dir: get_option('datadir') / 'icons')
