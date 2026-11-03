/*
 * The GObject subclass behind the settings backend (src/settings/mod.rs). It does only the GObject work: it forwards
 * each GSettingsBackend virtual method to Rust with the state pointer it was given, and calls the state's destructor
 * when the object is finalized. Storage, policy and notifications are in Rust (src/settings/backend.rs).
 *
 * The backend is given to gio::Settings directly, so it is not registered as a GIO module and nothing is selected by
 * the environment.
 */
#include <gio/gio.h>

/* The backend API is public but must be opted into before its header is included. */
#define G_SETTINGS_ENABLE_BACKEND
#include <gio/gsettingsbackend.h>

typedef void (*RsimgvStateDestroy) (void *state);

typedef struct _RsimgvSettingsBackend RsimgvSettingsBackend;
typedef struct _RsimgvSettingsBackendClass RsimgvSettingsBackendClass;

struct _RsimgvSettingsBackend {
  GSettingsBackend parent;
  void *state;
  RsimgvStateDestroy state_destroy;
};

struct _RsimgvSettingsBackendClass {
  GSettingsBackendClass parent_class;
};

/* Implemented in Rust (src/settings/backend.rs). The state is borrowed; the caller keeps it alive. */
GVariant *rsimgv_state_read (void *state, const gchar *key, const GVariantType *type);
gboolean rsimgv_state_get_writable (void *state, const gchar *key);
gboolean rsimgv_state_is_writable (void *state);
gboolean rsimgv_state_write (void *state, const gchar *key, GVariant *value, gpointer origin_tag);
gboolean rsimgv_state_write_tree (void *state, const gchar *path, const gchar * const *keys,
                                  GVariant * const *values, gpointer origin_tag);
void rsimgv_state_reset (void *state, const gchar *key, gpointer origin_tag);
void rsimgv_state_sync (void *state);

#define STATE(backend) (((RsimgvSettingsBackend *) (backend))->state)

G_DEFINE_TYPE (RsimgvSettingsBackend, rsimgv_settings_backend, G_TYPE_SETTINGS_BACKEND)

static GVariant *
rsimgv_backend_read (GSettingsBackend *backend, const gchar *key, const GVariantType *expected_type,
                     gboolean default_value)
{
  return rsimgv_state_read (STATE (backend), key, expected_type);
}

static GVariant *
rsimgv_backend_read_user_value (GSettingsBackend *backend, const gchar *key, const GVariantType *expected_type)
{
  return rsimgv_state_read (STATE (backend), key, expected_type);
}

static gboolean
rsimgv_backend_get_writable (GSettingsBackend *backend, const gchar *key)
{
  return rsimgv_state_get_writable (STATE (backend), key);
}

static GPermission *
rsimgv_backend_get_permission (GSettingsBackend *backend, const gchar *path)
{
  return g_simple_permission_new (rsimgv_state_is_writable (STATE (backend)));
}

static gboolean
rsimgv_backend_write (GSettingsBackend *backend, const gchar *key, GVariant *value, gpointer origin_tag)
{
  return rsimgv_state_write (STATE (backend), key, value, origin_tag);
}

static gboolean
rsimgv_backend_write_tree (GSettingsBackend *backend, GTree *tree, gpointer origin_tag)
{
  gchar *path = NULL;
  const gchar **keys = NULL;
  GVariant **values = NULL;
  gboolean written;

  /* The keys are relative to the path, and the arrays are NULL-terminated. */
  g_settings_backend_flatten_tree (tree, &path, &keys, &values);
  written = rsimgv_state_write_tree (STATE (backend), path, keys, values, origin_tag);
  g_free (path);
  g_free (keys);
  g_free (values);
  return written;
}

static void
rsimgv_backend_reset (GSettingsBackend *backend, const gchar *key, gpointer origin_tag)
{
  rsimgv_state_reset (STATE (backend), key, origin_tag);
}

/* Every key is watched by its file, so there is nothing to subscribe to. */
static void
rsimgv_backend_subscribe (GSettingsBackend *backend, const gchar *name)
{
}

static void
rsimgv_backend_unsubscribe (GSettingsBackend *backend, const gchar *name)
{
}

static void
rsimgv_backend_sync (GSettingsBackend *backend)
{
  rsimgv_state_sync (STATE (backend));
}

static void
rsimgv_settings_backend_finalize (GObject *object)
{
  RsimgvSettingsBackend *self = (RsimgvSettingsBackend *) object;

  if (self->state_destroy)
    self->state_destroy (self->state);
  self->state = NULL;
  G_OBJECT_CLASS (rsimgv_settings_backend_parent_class)->finalize (object);
}

static void
rsimgv_settings_backend_class_init (RsimgvSettingsBackendClass *klass)
{
  GObjectClass *object_class = G_OBJECT_CLASS (klass);
  GSettingsBackendClass *backend_class = (GSettingsBackendClass *) klass;

  object_class->finalize = rsimgv_settings_backend_finalize;
  backend_class->read = rsimgv_backend_read;
  backend_class->read_user_value = rsimgv_backend_read_user_value;
  backend_class->get_writable = rsimgv_backend_get_writable;
  backend_class->get_permission = rsimgv_backend_get_permission;
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
}

/*
 * Creates the backend around `state`, which the object owns from now on: `state_destroy` is called with it when the
 * object is finalized. The returned reference is the caller's.
 */
GSettingsBackend *
rsimgv_settings_backend_new (void *state, RsimgvStateDestroy state_destroy)
{
  RsimgvSettingsBackend *self = g_object_new (rsimgv_settings_backend_get_type (), NULL);

  self->state = state;
  self->state_destroy = state_destroy;
  return G_SETTINGS_BACKEND (self);
}
