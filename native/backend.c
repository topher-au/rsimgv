/*
 * A GSettingsBackend that stores settings in a YAML file. The GObject subclass lives here; the
 * storage (parsing, writing, and watching the file) is in Rust, reached through the rsimgv_store_*
 * functions. The backend is registered as "yaml" and selected with GSETTINGS_BACKEND=yaml.
 *
 * Rust cannot declare this subclass: glib-rs keeps the trait that makes a foreign GObject type
 * subclassable private, and GSettingsBackend is such a type.
 */
#include <gio/gio.h>

/* The backend API is public but must be opted into before its header is included. */
#define G_SETTINGS_ENABLE_BACKEND
#include <gio/gsettingsbackend.h>

typedef struct _RsimgvSettingsBackend RsimgvSettingsBackend;
typedef struct _RsimgvSettingsBackendClass RsimgvSettingsBackendClass;

struct _RsimgvSettingsBackend {
  GSettingsBackend parent;
};

struct _RsimgvSettingsBackendClass {
  GSettingsBackendClass parent_class;
};

/* Implemented in Rust (src/settings/store.rs). */
void rsimgv_store_init (GSettingsBackend *backend);
GVariant *rsimgv_store_read (const gchar *key, const GVariantType *type);
gboolean rsimgv_store_write (const gchar *key, GVariant *value);
void rsimgv_store_reset (const gchar *key);
void rsimgv_store_sync (void);

G_DEFINE_TYPE (RsimgvSettingsBackend, rsimgv_settings_backend, G_TYPE_SETTINGS_BACKEND)

static GVariant *
rsimgv_backend_read (GSettingsBackend   *backend,
                    const gchar        *key,
                    const GVariantType *expected_type,
                    gboolean            default_value)
{
  return rsimgv_store_read (key, expected_type);
}

static GVariant *
rsimgv_backend_read_user_value (GSettingsBackend   *backend,
                               const gchar        *key,
                               const GVariantType *expected_type)
{
  return rsimgv_store_read (key, expected_type);
}

static gboolean
rsimgv_backend_get_writable (GSettingsBackend *backend,
                            const gchar      *key)
{
  return TRUE;
}

static gboolean
rsimgv_backend_write (GSettingsBackend *backend,
                     const gchar      *key,
                     GVariant         *value,
                     gpointer          origin_tag)
{
  if (!rsimgv_store_write (key, value))
    return FALSE;
  g_settings_backend_changed (backend, key, origin_tag);
  return TRUE;
}

static gboolean
write_one (gpointer key, gpointer value, gpointer user_data)
{
  rsimgv_store_write ((const gchar *) key, (GVariant *) value);
  return FALSE; /* keep going */
}

static gboolean
rsimgv_backend_write_tree (GSettingsBackend *backend,
                          GTree            *tree,
                          gpointer          origin_tag)
{
  g_tree_foreach (tree, write_one, NULL);
  g_settings_backend_changed_tree (backend, tree, origin_tag);
  return TRUE;
}

static void
rsimgv_backend_reset (GSettingsBackend *backend,
                     const gchar      *key,
                     gpointer          origin_tag)
{
  rsimgv_store_reset (key);
  g_settings_backend_changed (backend, key, origin_tag);
}

static void
rsimgv_backend_subscribe (GSettingsBackend *backend,
                         const gchar      *name)
{
}

static void
rsimgv_backend_unsubscribe (GSettingsBackend *backend,
                           const gchar      *name)
{
}

static void
rsimgv_backend_sync (GSettingsBackend *backend)
{
  rsimgv_store_sync ();
}

static void
rsimgv_settings_backend_class_init (RsimgvSettingsBackendClass *klass)
{
  GSettingsBackendClass *backend_class = (GSettingsBackendClass *) klass;

  backend_class->read = rsimgv_backend_read;
  backend_class->read_user_value = rsimgv_backend_read_user_value;
  backend_class->get_writable = rsimgv_backend_get_writable;
  backend_class->write = rsimgv_backend_write;
  backend_class->write_tree = rsimgv_backend_write_tree;
  backend_class->reset = rsimgv_backend_reset;
  backend_class->subscribe = rsimgv_backend_subscribe;
  backend_class->unsubscribe = rsimgv_backend_unsubscribe;
  backend_class->sync = rsimgv_backend_sync;
}

static void
rsimgv_settings_backend_init (RsimgvSettingsBackend *self)
{
  rsimgv_store_init (G_SETTINGS_BACKEND (self));
}

/* Makes the backend available as "yaml". Call before the first GSettings object is created. */
void
rsimgv_settings_backend_register (void)
{
  /* GIO registers its extension points lazily; register this one so implementing it works. */
  g_io_extension_point_register (G_SETTINGS_BACKEND_EXTENSION_POINT_NAME);
  g_io_extension_point_implement (G_SETTINGS_BACKEND_EXTENSION_POINT_NAME,
                                  rsimgv_settings_backend_get_type (),
                                  "yaml",
                                  1000);
}
