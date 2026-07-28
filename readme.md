# bspwm-rs

a behavior-compatible rust ai rewrite of [bspwm](https://github.com/baskerville/bspwm).

documentation that works for bspwm should work for bspwm-rs

you should even be able to use bspc with bspwm-rs with no issue.

it should act mostly the same. there are some implementation differences ofc.

i also supposedly added support from some more ewmh extensions.

please tell me / open issue if you find incompatiblities with bspwm or any bugs and ill try to fix it

i might add some backwards compatible features (more commands) to it in the future. not sure

supposedly https://github.com/baskerville/bspwm/issues/651 issue should be fixed here (hidden window thing maybe?)

i am using this wm currently! it is ready for use! i don't use every bspwm feature though so im sure there are problems.

## installation

i might setup builds and releases later but i dont really want to rn

default binary names are `bspwm-rs` and `bspc-rs`

i just use the makefile

```sh
make build
sudo make install
```

## tests

theres a cool like xephyr based harness that makes your screen flash pretty colors in tests/
