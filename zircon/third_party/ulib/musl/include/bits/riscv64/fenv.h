#define FE_INEXACT 0x1
#define FE_UNDERFLOW 0x2
#define FE_OVERFLOW 0x4
#define FE_DIVBYZERO 0x8
#define FE_INVALID 0x10
#define FE_ALL_EXCEPT (FE_INEXACT | FE_UNDERFLOW | FE_OVERFLOW | FE_DIVBYZERO | FE_INVALID)

#define FE_TONEAREST 0
#define FE_TOWARDZERO 1
#define FE_DOWNWARD 2
#define FE_UPWARD 3

typedef unsigned int fexcept_t;

typedef struct {
  unsigned int __fpcr;
} fenv_t;

#define FE_DFL_ENV ((const fenv_t*)-1)
