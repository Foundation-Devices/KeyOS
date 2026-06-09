// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#include "string.h"
#include <stdarg.h>
#include <stdio.h>

void *memcpy(void *dest, const void *src, size_t len)
{
	char *d;
	const char *s;
	struct chunk {
		unsigned long val[2];
	};

	const struct chunk *csrc = (const struct chunk *) src;
	struct chunk *cdst = (struct chunk *)dest;

	if (((unsigned long)src & 0xf) == 0 && ((unsigned long)dest & 0xf) == 0) {
		while (len >= sizeof(struct chunk)) {
			*cdst++ = *csrc++;
			len -= sizeof(struct chunk);
		}
	}

	d = (char *) cdst;
	s = (const char *) csrc;

	while (len--)
		*d++ = *s++;

	return dest;
}

void *memset(void *dest, int val, size_t len)
{
	char *d = (char *)dest;

	while (len--)
		*d++ = (char)val;

	return dest;
}

int memcmp(const void *dest, const void *src, size_t len)
{
	const char *d = (const char *)dest;
	const char *s = (const char *)src;
	int r = 0;

	while (len-- && (r = *d++ - *s++) == 0) ;

	return r;
}

void *memchr(const void *src, int val, size_t len)
{
	char *p = NULL;
	char *s = (char *)src;

	while (len) {
		if (*s == val) {
			p = s;
			break;
		}
		s++;
		len--;
	}

	return p;
}

void *memmove(void *dest, const void *src, size_t len)
{
	char *p, *s;

	if (dest <= src) {
		p = (char *)dest;
		s = (char *)src;
		while (len--)
			*p++ = *s++;
		}
	else {
		p = (char *)dest + len;
		s = (char *)src + len;
		while (len--)
			*--p = *--s;
		}

	return dest;
}

void explicit_bzero(void *s, size_t len)
{
    memset(s, '\0', len);
}

size_t strlen(const char *str)
{
	size_t i = 0;

	while (str[i++] != '\0') ;

	return i - 1;
}

size_t strnlen(const char *str, size_t maxlen)
{
  const char *found = memchr (str, '\0', maxlen);
  return found ? found - str : maxlen;
}

char *strcpy(char *dest, const char *src)
{
	char *bak = dest;

	while ((*dest++ = *src++) != '\0') ;

	return bak;
}

char *strncpy(char *s1, const char *s2, size_t n)
{
  size_t size = strnlen (s2, n);
  if (size != n)
    memset (s1 + size, '\0', n - size);
  return memcpy (s1, s2, size);
}

size_t strlcpy(char *dest, const char *src, size_t len)
{
	size_t i = 0;
    while (i < len - 1 && src[i] != '\0') {
        dest[i] = src[i];
        i++;
    }
    if (len > 0) {
        dest[i] = '\0';
    }
    while (src[i] != '\0') {
        i++;
    }
    return i;
}

char *strcat(char *dest, const char *src)
{
  strcpy (dest + strlen (dest), src);
  return dest;
}

size_t strlcat(char *dest, const char *src, size_t size)
{
  size_t src_length = strlen (src);

  if (size == 0)
    return src_length;

  size_t dest_length = strnlen (dest, size);
  if (dest_length != size)
  {
    size_t to_copy = size - dest_length - 1;
    if (to_copy > src_length)
      to_copy = src_length;

    char *target = dest + dest_length;
    memcpy (target, src, to_copy);
    target[to_copy] = '\0';
  }

  return dest_length + src_length;
}

int strcmp(const char *p1, const char *p2)
{
	unsigned char c1, c2;

	while (1) {
		c1 = *p1++;
		c2 = *p2++;
		if (c1 != c2)
			return c1 < c2 ? -1 : 1;
		if (!c1)
			break;
	}

	return 0;
}

int strncmp(const char *p1, const char *p2, size_t len)
{
	unsigned char c1, c2;

	while (len--) {
		c1 = *p1++;
		c2 = *p2++;

		if (c1 != c2)
			return c1 < c2 ? -1 : 1;

		if (!c1)
			break;
	}

	return 0;
}

char *strchr(const char *s, int c)
{
	for (; *s != (char) c; ++s)
		if (*s == '\0')
			return NULL;

	return (char *)s;
}

int sprintf(char *str, const char *format, ...)
{
    va_list ap;
    va_start(ap, format);
    int n = vsnprintf(str, (size_t)-1, format, ap);
    va_end(ap);
    return n;
}
