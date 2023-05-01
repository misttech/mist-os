        if (callback && cookie) {{
            struct AsyncCallbackWrapper {{
                const void* driver;
                {protocol_c_name}_{method_c_name}_callback callback;
                void* cookie;
            }};

            AsyncCallbackWrapper* wrapper = new AsyncCallbackWrapper {{
                fdf_env_get_current_driver(),
                callback,
                cookie,
            }};

            cookie = wrapper;
            callback = []({params}) {{
                AsyncCallbackWrapper* wrapper = static_cast<AsyncCallbackWrapper*>(ctx);
                fdf_env_register_driver_entry(wrapper->driver, runtime_enforce_no_reentrancy);
                wrapper->callback({args});
                fdf_env_register_driver_exit();
                delete wrapper;
            }};
        }}
