#include <mach/mach.h>
#include <servers/bootstrap.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>

struct spindle_mach_message {
  mach_msg_header_t header;
  mach_msg_size_t descriptor_count;
  mach_msg_ool_descriptor_t descriptor;
};

int32_t spindle_sketchybar_send(const char *bar_name,
                                const uint8_t *message,
                                uint32_t message_len) {
  if (!bar_name || !message) {
    return KERN_INVALID_ARGUMENT;
  }

  char service_name[256];
  int written = snprintf(service_name,
                         sizeof(service_name),
                         "git.felix.%s",
                         bar_name);
  if (written < 0 || (size_t)written >= sizeof(service_name)) {
    return KERN_INVALID_ARGUMENT;
  }

  mach_port_t port = MACH_PORT_NULL;
  kern_return_t lookup = bootstrap_look_up(bootstrap_port, service_name, &port);
  if (lookup != KERN_SUCCESS) {
    return lookup;
  }

  struct spindle_mach_message msg = {0};
  msg.header.msgh_remote_port = port;
  msg.header.msgh_bits = MACH_MSGH_BITS_SET(MACH_MSG_TYPE_COPY_SEND
                                            & MACH_MSGH_BITS_REMOTE_MASK,
                                            0,
                                            0,
                                            MACH_MSGH_BITS_COMPLEX);
  msg.header.msgh_size = sizeof(struct spindle_mach_message);
  msg.descriptor_count = 1;
  msg.descriptor.address = (void *)message;
  msg.descriptor.size = message_len;
  msg.descriptor.copy = MACH_MSG_VIRTUAL_COPY;
  msg.descriptor.deallocate = false;
  msg.descriptor.type = MACH_MSG_OOL_DESCRIPTOR;

  return mach_msg(&msg.header,
                  MACH_SEND_MSG,
                  sizeof(struct spindle_mach_message),
                  0,
                  MACH_PORT_NULL,
                  MACH_MSG_TIMEOUT_NONE,
                  MACH_PORT_NULL);
}
