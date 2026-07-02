<!--
SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->
# Slint Platform File Based Routing

## Rules of Routing

1. **Page Location**: Pages must be defined in a file named `page.slint` and be the last export in that file.

2. **Props Struct**: Each page must have at least one input property, which is a `props struct` annotated with the `route` rust attribute.

3. **No Recursive Imports**: A page and its corresponding `props struct` cannot be defined in the same file, due to slint not supporting recursive imports

4. **Property Accessibility**: A page can access the `props structs` of its parent routes, but not those of its siblings.

5. **Unique Route Bindings**: All `route bindings` (variables in the path) must have unique names within a given route.

6. **Default Page**: Exactly one page must be marked as the `default` page using the `default` flag in the `route` attribute. This page will be the first one mounted when the application starts.

7. **Include Exports**: In your root app file, add `export * from "gen/exports.slint";` to access navigation callbacks and state from Rust.

8. **Development Mode**: During development, you can use the `dev` flag in the `route` attribute to exclusively mount specific pages. If any page has the `dev` flag enabled, only those pages will be mounted.

## setup

#### cargo.toml
add the following dependencies
- serde
- slint-keyos-platform

```toml

[dependencies]
serde = { version = "1.0" }
slint-keyos-platform = { path = "../../slint-keyos-platform" }

```

#### build.rs
In your `build.rs` enable `include_router` flag

```rust
fn main() {
    slint_keyos_platform::compile_options(slint_keyos_platform::CompileOptions {
        module_path: "ui/app.slint",
        include_router: true,
        include_slint: true,
    });
}
```


#### /ui/app.slint

Once codgen has run once, you should mount the generated router in your main component (e.g. `ui/app.slint`)

Also include the navigation re-export at the bottom, so that the routing globals are accessible from rust code.

```rust 
import { Router } from "gen/router.slint";

export component AppWindow inherits Window {
    Router {}
}

export * from "gen/exports.slint";

```

#### /src/main.rs

The router is set up automatically by the `app!` macro, no need to do
anything else.

## Overview 

Sample Project Structure

```
├── /src
│   └── main.rs
│
├── /ui
│   ├── /pages
│   │   ├── /one
│   │   │   ├── page.slint
│   │   │   └── props.slint
│   │   │
│   │   └── /two
│   │       ├── page.slint
│   │       └── props.slint
│   │
│   ├── app.slint
│   │
│   └── /keyos (codegen output)
│       ├── exports.slint (all exports will be here!) 
│       ├── internal.slint
│       ├── navigate.slint
│       └── router.slint
│
├── Cargo.toml
└── build.rs
```

## Props

Each page must have a corresponding `props struct`. This enables passing values between pages in a type-safe manner.

The structs should be annotated with `@rust-attr(route(...))`

#### /pages/one/props.slint 
This page will be considered the default page, due to the inclusion of the `default` flag in the `route` attribute. 

```rust
@rust-attr(route(default, path = "/one/"))
export struct OneProps {}
```

#### /pages/one/props.slint
This struct has variables included in the route path. Each struct field must be included in the route path.

```rust
@rust-attr(route(path = "/two/{name}/{age}"))
export struct TwoProps {
    name: string,
    age: int
}
```

#### Complex Type

You can include complex types (structs and enums) in routes, but they must derive `serde::Serialize` and `serde::Deserialize`.

Here's an example

```rust

@rust-attr(derive(serde::Serialize, serde::Deserialize))
export enum Option {
    Overview,
    General,
    Advanced
}

@rust-attr(route(path = "/settings/info/{page}"))
export struct SettingsInfoProps {
    page: Option
}
```

## Pages

Pages must be placed inside `page.slint`. Ideally their props would live in the neighbor `props.slint`. 

> `Props structs` and pages CANNOT be defined in the same slint file. 

Pages should have their props as a public `in` or `in-out` property. A property must have the `@rust-attr` route annotation as described before.

`in property <TYPE> <any-prop-name>`

The simplest possible page would look something like this

#### /pages/one/page.slint
```rust
import { OneProps } from "./props.slint";

export component PageOne {
    in property <OneProps> props;
    Text {
        text: "Page One";
    }
}
```

## Codegen

The two slint files that are generated are
- router.slint
- navigate.slint

#### router.slint
Exports a Router component that mounts the active page. You must mount this component into the root of your app.

#### navigate.slint
Exports: 
- Navigation State properties
- Callbacks for managing navigation history
- Callbacks for type safe navigation between pages

Here's what the navigation file looks like for the example project:

```rust
export global Navigation {
    // Navigation state
    in property <bool> has-backward;
    in property <bool> has-forward;
    
    // Generic navigation callbacks
    callback backward();
    callback forward();
    
    // Page-specific navigation callbacks
    callback page-one({  });
    callback page-two({ name: string, age: int });
}
```

You can then import the `Navigation` global from any of the pages to navigate between pages. Here's an example of navigating from `PageOne` to `PageTwo`.


#### /pages/one/page.slint
```rust
import { Button } from "std-widgets.slint";
import { OneProps } from "./props.slint";
import { Navigation } from "../../navigate.slint";

export component PageOne {
    in property <OneProps> props;
    Text {
        text: "Page One";
    }
    Button {
        text: "Overview";
        clicked => {
            Navigation.page-two({ name: "mike", age: 42 })
        }
    }
}
```

## Nested Routing

You can nest routes, enabling common route state between related routes. 

In this example I've moved `/two` under `/one`

```
.
├── /src
│   └── ...
│
├── /ui
│   ├── /pages
│   │   └── /one
│   │       ├── page.slint
│   │       ├── props.slint
│   │       │
│   │       └── /two
│   │           ├── page.slint
│   │           └── props.slint
│   │
│   └── /keyos (codegen output)
│       └── ... 
│
├── Cargo.toml
└── build.rs
```

Given the path for PageOne is `/one/`, the path for PageTwo will be `/one/two/{name}/{age}`

The paths are concatenated from parent to child. 

In your page component, you are allowed to have properties for any of the parent routes. Meaning PageTwo could have two sets of properties. 

**IMPORTANT** You can only access properties of a parent directory, or of your current directory (where page.slint is defined)

```rust
import { OneProps } from "../props.slint";
import { TwoProps } from "./props.slint";

export component PageTwo {
    // Parent props are optional. 
    // It is implied based on file/directory structure if it is not explicitly included.
    in property <OneProps> one;
    in property <TwoProps> two;
    Text {
        text: "Page One";
    }
}
```

Another rule is that all route bindings (a.k.a the variables in path strings) need to be unique by key, for a given route.

So if we had two different `prop structs`, they cannot both have a variable named `age`.

```rust

@rust-attr(route(path = "/one/{age}"))
export struct OneProps {
    age: int
}

@rust-attr(route(path = "/two/{age}"))
export struct TwoProps {
    age: int
}

```

The following error would be shown

```shell
× Route validation errors

  Error:   × Duplicate field key: age
    help: Ensure unique field keys in page properties

  Error:   × Field: age
     ╭─[/keyOS/apps/gui-app-example-routing/ui/pages/one/props.slint:5:25]
   4 │ @rust-attr(route(path = "/one/{age}"))
   5 │ export struct OneProps {
     ·                         ▲
     ·                         ╰── First occurrence
   6 │     age: int
     ╰────

  Error:   × Field: age
     ╭─[/keyOS/apps/gui-app-example-routing/ui/pages/one/two/props.slint:6:26]
   5 │ @rust-attr(route(path = "/two/{age}"))
   6 │ export struct TwoProps {
     ·                         ▲
     ·                         ╰── Duplicate key
   7 │     age: int
     ╰────
```
