# bspwm-rs

a behavior-compatible rust ai rewrite of [bspwm](https://github.com/baskerville/bspwm).

it should act mostly the same. there are some implementation differences tho

please tell me / open issue if you find incompatiblities with bspwm ill try to fix it

i might add some backwards compatible features (more commands) to it in the future. not sure

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
