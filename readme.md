# bspwm-rs

a behavior-compatible rust ai rewrite of [bspwm](https://github.com/baskerville/bspwm). you can be happy that none of this readme is ai at least.

i made it because i wanted to fix long standing issues and bugs with bspwm, and some of these could be delivered through rust language features. the rust binary is like multiple times larger, it's quite a shame.

documentation that works for bspwm should work for bspwm-rs. you should even be able to use bspc with bspwm-rs with no issue. it should act mostly the same. there are some implementation differences ofc.

i also supposedly added support from some more ewmh extensions.

please tell me / open issue if you find incompatiblities with bspwm or any bugs and ill try to fix it

i might add some backwards compatible features (more commands) to it in the future. not sure

i am using this wm currently! it is ready for use! i don't use every bspwm feature though so im sure there are problems.


## supposedly fixed issues / extra features

https://github.com/baskerville/bspwm/issues/651 if the issue really was that things were getting placed below hidden windows (you can see my comment)

https://github.com/baskerville/bspwm/pull/1362 should be fixed here, because the serde json encoder should do this correctly

we have support for _NET_WM_MOVERESIZE https://github.com/baskerville/bspwm/pull/1183

## extra features and major differences

i added support for syncronization in resize. i dont know what really supports this though? need to test more

### stacking

i also changed the underlying stacking logic. the hope is the new logic is more maintainable.

it might cause some incompatiblities with how bspwm does stacking. please point these out and i can attempt to address them.

to be honest, i have been frustrated with bspwm floating stacking logic for a long time. i use the window manager hybrid tiled and floating, with windows floating by default. my hope is that this stacking backend will allow for many of my issues to be resolved.

## installation

i might setup builds and releases later but i dont really want to rn

default binary names are `bspwm-rs` and `bspc-rs`

i just use the makefile

```sh
make build
sudo make install
```

## wayland

i have **no plans** to add wayland support to this. i have been working on a separate wayland wm that is compatible with bspwm semantics, but it is written in go and it is very much for fun.

i would be open to somebody contributing wayland support to this, it would be nice if it was someone who actually uses that accursed protocol. i think it would be cool to expose keyboard events over some secure socket for daemon applications to listen on.

## tests

theres a cool like xephyr based harness that makes your screen flash pretty colors in tests/
