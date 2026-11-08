/* Bridges GTK widgets to PyGObject without linking libpygobject.
 *
 * PyGObject's C API is a function table in the gi._gobject module. Calling
 * pygobject_init sets this translation unit's _PyGObject_API pointer, after
 * which pygobject_new and pygobject_get work. The symbols here are hidden so
 * they cannot collide with the copies inside the gi extension modules.
 */

#include <pygobject.h>
#include <gtk/gtk.h>

static int rsimgv_pygobject_ready = 0;

int rsimgv_pygobject_init(void)
{
    if (rsimgv_pygobject_ready) {
        return 1;
    }
    if (pygobject_init(-1, -1, -1) == NULL) {
        return 0;
    }
    rsimgv_pygobject_ready = 1;
    return 1;
}

PyObject *rsimgv_widget_to_py(GtkWidget *widget)
{
    if (widget == NULL) {
        return NULL;
    }
    return pygobject_new(G_OBJECT(widget));
}

GtkWidget *rsimgv_py_to_widget(PyObject *object)
{
    GObject *gobject;

    if (object == NULL || !PyObject_TypeCheck(object, &PyGObject_Type)) {
        return NULL;
    }
    gobject = pygobject_get(object);
    if (gobject == NULL || !GTK_IS_WIDGET(gobject)) {
        return NULL;
    }
    return GTK_WIDGET(gobject);
}
