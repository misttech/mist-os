{protocol_docs}
template <typename D, bool runtime_enforce_no_reentrancy = false>
class {protocol_name}Protocol : public internal::base_mixin {{
public:
    {protocol_name}Protocol() {{
        {protocol_name_snake}_protocol_server_driver_ = fdf_env_get_current_driver();
        internal::Check{protocol_name}ProtocolSubclass<D>();
{constructor_definition}
    }}

    const void* {protocol_name_snake}_protocol_server_driver() const {{
        return {protocol_name_snake}_protocol_server_driver_;
    }}

protected:
    {protocol_name_snake}_protocol_ops_t {protocol_name_snake}_protocol_ops_ = {{}};
    const void* {protocol_name_snake}_protocol_server_driver_;

private:
    static const void* GetServerDriver(void* ctx) {{
        return static_cast<D*>(ctx)->{protocol_name_snake}_protocol_server_driver();
    }}

{protocol_definitions}
}};

class {protocol_name}ProtocolClient {{
public:
    {protocol_name}ProtocolClient()
        : ops_(nullptr), ctx_(nullptr) {{}}
    {protocol_name}ProtocolClient(const {protocol_name_snake}_protocol_t* proto)
        : ops_(proto->ops), ctx_(proto->ctx) {{}}

    void GetProto({protocol_name_snake}_protocol_t* proto) const {{
        proto->ctx = ctx_;
        proto->ops = ops_;
    }}
    bool is_valid() const {{
        return ops_ != nullptr;
    }}
    void clear() {{
        ctx_ = nullptr;
        ops_ = nullptr;
    }}

{client_definitions}
private:
    const {protocol_name_snake}_protocol_ops_t* ops_;
    void* ctx_;
}};
