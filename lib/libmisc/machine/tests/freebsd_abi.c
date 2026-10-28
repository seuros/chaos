#include <sys/types.h>
#include <sys/ioctl.h>
#include <dev/acpica/acpiio.h>
#include <stddef.h>
#include <stdio.h>

#if __has_include(<drm.h>)
#include <drm.h>
#define HAVE_DRM_HEADERS 1
#else
#define HAVE_DRM_HEADERS 0
#endif

int
main(void)
{
  printf("%zu %zu %zu %zu %lu %lu %d",
      sizeof(struct acpi_battinfo),
      sizeof(union acpi_battery_ioctl_arg),
      _Alignof(union acpi_battery_ioctl_arg),
      offsetof(struct acpi_battinfo, state),
      (unsigned long)ACPIIO_BATT_GET_UNITS,
      (unsigned long)ACPIIO_BATT_GET_BATTINFO, HAVE_DRM_HEADERS);
#if HAVE_DRM_HEADERS
  printf(" %zu %zu %zu %zu %lu %lu",
      sizeof(struct drm_mode_card_res),
      sizeof(struct drm_mode_get_connector),
      offsetof(struct drm_mode_get_connector, connection),
      sizeof(struct drm_mode_modeinfo),
      (unsigned long)DRM_IOCTL_MODE_GETRESOURCES,
      (unsigned long)DRM_IOCTL_MODE_GETCONNECTOR);
#endif
  putchar('\n');
  return (0);
}
